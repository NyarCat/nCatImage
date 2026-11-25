# 03. 작은 Rust 구조와 작업 수명

v0.4.3 · 2026-11-16. 목표 계약과 현재 구현 차이는 [08](08-implementation-status.md)에서 구분한다.

## 1. 기술 선택과 경계

현재 구현 대상은 Linux x64. 앱/backend/worker/GUI 연결은 Rust다. GUI는 **gtk-rs/GTK4**의 기본 위젯·GDK texture 표시부터 PoC한다. libadwaita는 필수 의존성이 아니며 필요한 native UX와 배포/메모리 비용을 비교한 뒤 추가 여부를 결정한다.

이유는 Linux native 입력·IME·접근성을 활용하면서 별도 웹 runtime·custom GPU engine을 처음부터 만들지 않기 위해서다. GTK가 가장 가볍다는 결론은 아직 없으며 07의 빈 창/페이지/반응성 목표로 실제 판정한다.

초기 Linux 범위에서는 다중 OS GUI·C#/Swift 셸·범용 C ABI·async runtime·DI container·플러그인 시스템·DB·자체 renderer를 제외했다. 이후 사용자 요청으로 Windows native W0 bootstrap을 추가하며 범위는 11을 따른다. 불필요한 범용 계층은 도입하지 않는다.

## 2. 코드 구조 — Windows W0 추가 후 11개 crate

```text
Cargo.toml / Cargo.lock / rust-toolchain.toml
crates/
  ncat-core/       ID·자연 정렬·읽기 페이지/spread·ViewPlan·예산 규칙
  ncat-presentation/ 툴킷 독립 GUI 명령·읽기 상태·보기/입력 정책
  ncat-gtk/        GTK 화면·입력 매핑·픽셀 표시·비동기 작업 수명
  ncat-desktop/    향후 Windows/macOS용 로컬 GTK 진입점 (native port 미완료)
  ncat-windows/    단일 EXE Win32 bootstrap·Wine 검사 (뷰어/보호 미완료)
  ncat-platform/   Unix 파일 리비전/권한/open·Linux worker 보호
  ncat-protocol/   worker 요청/응답·버전·오류·상한 DTO
  ncat-backend/    세션·scheduler·IPC client·cache·설정/진단
  ncat-media/      코덱·색관리·압축 adapter, worker에서만 사용
  ncat-worker/     비신뢰 입력 처리 실행 파일
  ncat-linux/      Linux 진입점·KIOFuse/GVfs 인증/마운트 어댑터
tests/corpus/     재배포 가능한 경계 파일·hash·생성기
```

기존 6개 crate에 presentation·GTK frontend·platform을 추출했다. 재사용 범위와 미구현 OS 기능은 [09](09-portability.md)를 따른다. 현재 회귀 입력은 worker 통합 테스트가 temp 디렉터리에 생성하고, 수동 GTK 입력은 `tools/smoke-fixtures.py`로 생성한다. `tests/corpus/`의 정식 corpus는 아직 없다. 아래의 prefetch/협력 취소/유휴 회수 등 전체 계약은 목표이며 [08](08-implementation-status.md)에 실제 차이를 기록한다.

- core는 UI·codec·OS·Tokio 비의존, `forbid(unsafe_code)`다.
- Windows 단일 EXE/Wine W0 범위는 [11](11-windows-single-exe-wine.md)을 따른다. 직접 Win32 호출은 native 셸의 `win32.rs`에만 격리하며 `deny(unsafe_op_in_unsafe_fn)`·handle/callback 수명 주석을 둔다. 공통 core/protocol/presentation/backend/media/worker의 unsafe 금지는 유지한다.
- Linux GUI는 현재 `Backend / Event / View / FrameBytes`로 조정·표시를 연결한다. `Action / Snapshot / FrameLease` 명칭의 전체 목표 API가 구현된 것은 아니다. media parser를 UI에 링크해 직접 호출하지 않는다.
- 읽기 입력/보기 정책은 presentation, 정렬·예산/spread는 core, scheduler/cache/worker는 backend, widgets·포커스·대화상자·texture와 요청/이벤트 수명 조정은 GTK frontend에 둔다. 완전한 toolkit-independent controller는 아직 아니다.
- media의 native FFI는 안전 조건·스레드·allocator·해제 함수를 좁게 캡슐화한다. `Send/Sync`를 근거 없이 수동 구현하지 않는다.

## 3. 실행 모델

- 기본은 **GUI 프로세스 + 지연 실행 worker 1개**다. worker는 소스/codec 세션을 재사용하고 동시에 비신뢰 작업 하나만 실행한다.
- GUI main thread는 native 이벤트/표시만 담당한다. backend 조정/IPC용 thread 하나를 기본으로 검증하고 CPU 작업을 UI에서 하지 않는다. 처음부터 Tokio/Rayon pool을 만들지 않는다.
- worker는 동기 decode 실행 중에도 취소 신호를 받을 수 있어야 한다. 최소 제어 reader/취소 플래그 경로를 별도 두고 제한 reader/디코더의 협력 취소 지점에서 확인한다. 제어 thread도 실제 thread 수에 포함한다. 취소 불가능한 native 호출은 parent의 kill/reap 경로로 처리한다.
- worker의 코덱 내부 스레드는 기본 1개를 요청하고 병렬화 불가 옵션/추가 스레드는 기록한다. 코덱 thread 수를 늘리는 것은 CPU/지연 실측 후 승인한다.
- worker 2개 모드는 기본값이 아니다. 한 worker로 전경 응답성을 못 지키는 재현 사례가 있을 때만 PoC하고 메모리/CPU 증가를 함께 보고한다. 총 bytes 예산은 늘리지 않는다.
- 한 worker에서 prefetch/썸네일은 전경 대기가 없을 때만 한 작업씩 한다. 전경이 생기면 대기 비필수 작업을 버리고 실행 중 작업에 협력 취소한다.
- 코덱/solid 읽기가 취소 안 되면 100ms 유예 후 **전경 구독자 없는 비필수 작업의 worker만** 회수하는 정책을 시험한다. worker 재시작 비용과 압축 세션 손실이 목표에 맞는지 M0에서 판정한다.
- thread/future drop을 코덱 종료로 간주하지 않는다. 실제 종료/reap 확인 후 슬롯/예약을 반환한다. worker kill로 잃은 세션/요청을 전부 무효화한다.

## 4. 요청·큐·버전

현재 v0.5.4 wire v5는 bounded `FrameInfo.metadata`를 추가한다. metadata는 worker가 추출하고 parent가 개수/길이/제어 문자를 검증하며 GUI는 plain text overlay로만 표시한다. animation/seek/cache의 frame 소유권·예산에도 연결한다. [12](12-metadata-overlay.md)를 따른다.

Windows W0에서 추가한 wire v4 경로 표현은 유지한다. Unix bytes와 Windows UTF-16LE code units를 같은 host의 부모/worker 간 lossless하게 전달하고 invalid NUL/length/alignment는 Deserialize 단계에서 거부한다. worker 명령은 executable/arguments로 분리하며 같은 EXE 모드는 명시적 `--worker` 인자만 사용한다. Windows codec 연결은 구현했지만 native DACL/Job·실사용 검증은 별도 미완료다.

v0.5.1 wire v3은 animation의 확인된 `total`(없으면 unknown)을 전달하고 `index < total <= MAX_ANIMATION_FRAMES`를 parent에서 검증한다. 번호 seek는 worker에서 한 프레임씩 합성/폐기하고 범위 초과 시 마지막 유효 프레임을 반환한다. 자동 재생은 애니메이션 툴바의 텍스트만 갱신하며 page spin/창 제목·이전/다음 버튼 sensitivity를 busy마다 갱신하지 않는다. 아래 wire v2 설명은 0.5.0 구현 이력이다.

프레임 UI는 `AnimationSeek`로 이전 프레임을 요청하며 worker가 동일한 예약 안에서 처음부터 해당 프레임까지 합성/폐기한다. pause·배속은 presentation 정책과 GTK one-shot timer에서 처리하고 미리 디코딩 큐는 추가하지 않는다. finite 종료 후에도 이전/재시작을 위해 session을 유지하며 다음 페이지·취소·종료에서 해제한다.

미릴리스 wire v2는 `AnimationNext`/`AnimationEnded`와 프레임 timing metadata를 추가한다. 최초 decode가 애니메이션이면 worker의 iterator/합성 canvas를 유지하고 parent `Permit`도 함께 유지한다. GTK는 표시한 프레임의 delay 뒤 one-shot timer로 다음 프레임 하나만 요청한다. 정상 정지 페이지에는 재생 timer가 없고, 전 프레임 수집/무한 predecode 큐는 없다. 애니메이션은 static LRU에 넣지 않으며 재생 중 인접 prefetch를 만들지 않는다. 새 페이지/보기는 기존 worker session을 회수해 새로 시작한다. GUI와 worker를 같은 wire 버전으로 배포해야 한다.

| 종류 | 초기 한도/동작 |
|---|---|
| 사용자 open/선택/view 상태 | 종류별 최신 1개로 병합 |
| 전경 작업 | 현재 spread에 필요한 항목, 한 worker에서 직렬 decode |
| 비필수 대기 | prefetch 2개·썸네일 8개, offscreen/방향 전환 시 취소 |
| 이벤트 | 제어 최대 128개/1MiB, terminal 슬롯 별도 예약 |
| 표시 결과 | 최신 spread의 패널당 1개 lease, 실제 pixel bytes는 05 예산 |

- 제어 큐에 pixel payload를 넣지 않는다. 진행 상태/preview는 병합하고 오류/종료/lease 회수 신호를 포화 때문에 잃지 않는다. 생산자가 UI를 블로킹하지 않게 역압/거부 정책을 둔다.
- 세대는 `BookEpoch / ContentRevision / SelectionVersion / ViewVersion`이다. 소스 교체·재읽기·페이지 이동·zoom 각각을 구분한다.
- 결과 수신·cache 삽입·texture 생성 직전에 필요한 버전을 검사한다. 이전 책/페이지/화면 설정의 결과를 재사용하지 않는다.
- cache/shared-work key는 정확한 소스·리비전·항목/내부 페이지·decode 조건/색 정책을 포함한다. render는 crop/화면 변환도 포함한다.
- shared work의 owner는 scheduler다. 한 요청자의 취소는 해당 구독만 해제하며 다른 전경/미리 읽기 수요를 확인한 뒤 실제 작업을 중단한다. 실패 작업을 영구 공유하지 않는다.

## 5. UI 깨우기와 표시

1. backend가 이벤트를 게시하고 빈 큐→비어 있지 않음 전환에서 main context를 깨운다. GTK 객체는 main thread에서만 접근한다.
2. GLib source/event 연동으로 UI가 dispatch당 최대 16개 또는 3ms까지만 drain한다. 남으면 다음 dispatch를 예약한다. drain/wake 플래그 경합으로 신호를 잃지 않게 재검사한다.
3. UI는 버전 확인 후 validated raw pixel을 GDK texture로 감싼다. `glib::Bytes` 등 backing owner가 FrameLease를 함께 유지해야 한다.
4. 새 표시로 바꾼 뒤 이전 texture를 반환하고 실제 마지막 owner/제출 수명에 맞춰 예산을 반납한다. GDK의 지연 GPU upload를 별도로 측정한다.
5. 정지 화면은 변경 없으면 redraw/polling하지 않는다. 키를 누르거나 마우스를 움직여야 decode 결과가 보이는 구조를 금지한다.

원본 전체 크기 texture를 항상 만들지 않는다. crop·표시 크기·타일 계획은 backend ViewPlan을 따른다. UI 자체에서 원본 파일을 decode하지 않는다.

## 6. 버퍼·IPC·오류

- `FrameLease = 불변 픽셀 owner + 차원/stride/색·alpha 정보 + 예산 permit`이다. Arc 복제는 같은 할당을 중복 과금하지 않되 copy/변환/staging은 별도 예약한다.
- cache 조회와 lease 획득은 원자적이다. 퇴출 후 화면이 쓰는 buffer는 살아 있고 예산도 유지한다.
- IPC는 전용 pipe의 길이 제한 DTO와 request ID/버전이다. 역직렬화 전 길이, 이후 문자열/배열/차원/stride를 검사한다. stdout IPC와 진단을 섞지 않는다.
- 큰 pixels는 권한 제한 공유 buffer/앱 임시 파일을 사용한다. pointer/JSON Base64는 보내지 않는다. worker 게시 후 불변성을 보장 못하면 부모 소유 buffer로 복사하고 예약한다.
- 메타/픽셀/압축 입력/worker 내부 workspace는 parent 전역 예산으로 배정한다. worker마다 독립 192MiB를 부여하지 않는다.
- 예상 오류는 typed Result로 반환한다. native crash/OOM/abort는 worker 종료로 처리한다. 같은 입력에 무제한 재시도하지 않는다. transport 장애만 현재 요청을 최대 1회 복구하며 손상/한도 초과는 사용자 재시도 전까지 유지한다.
- 기본 단일 작업 timeout은 15초이며 대형/solid도 진행 신호만으로 무한 연장하지 않는다. codec별 조정은 측정/승인한다. UX에서는 150ms부터 대기/취소를 보여 준다.

## 7. 종료·유휴

0.4.3의 실제 네트워크·진단 경계: `ncat-linux/src/network.rs`가 SMB/FTP/SFTP URI를 검증하고 세션 D-Bus의 KIOFuse `mountUrl` 또는 GIO/GVfs/FUSE에서 로컬 경로를 얻는다. 파일 내용·목록·코덱 처리는 기존 worker에 남긴다. GLib future와 별도 요청 번호로 결과를 연결하며 새 열기·페이지 이동·종료는 이전 future를 abort하고 backend generation을 무효화한다. 앱 연결 대기 60초와 worker 작업 제한 15초는 별도 단계이고 OS 마운트 자체의 취소는 보장하지 않는다.

GUI의 타입이 제한된 진단 이벤트와 coordinator의 진단 이벤트는 공용 메모리 기록에 합쳐진다. UI는 우클릭 메뉴에서 기록의 스냅샷을 표시하며 디스크에 쓰지 않는다. FUSE 최초 stat 변화는 catalog/frame 게시 전 새 worker로 최초 열기 한 번만 재시도한다. 기존 책의 리비전·캐시 검사는 유지한다. 자체 프로토콜/계정/다운로드 관리자나 추가 Tokio/thread pool은 도입하지 않았다.

crate 분리는 GUI 교체 가능성을 돕지만 Windows/macOS 지원 완료를 뜻하지 않는다. GTK뿐 아니라 Unix 경로·rustix·worker 보호/IPC·OS 연결·패키징도 이식해야 한다. 아래 협력 종료/유휴 정책은 목표이며 현재 구현 차이는 08을 따른다.

- `Running → Closing → Closed` 단방향이며 재호출에 안전하다. 새 요청을 거부하고 모든 세대를 무효화한다.
- UI source/wake/입력을 정리하고 작업 취소→worker 정상 종료→필요 시 kill/reap→lease/cache/IPC/temp 순으로 회수한다. worker와 GUI texture의 실수명을 확인한다.
- 총 종료 목표 1.5초 안에서 협력 유예/kill·기록 flush를 배정한다. UI main thread에서 join하지 않고 완료 이벤트로 닫는다. Drop에서 비동기 정리를 대신하지 않는다.
- 파일이 열려 있을 때 worker 세션을 재사용한다. 책을 닫고 빈 화면에서 10초 유휴면 worker를 회수한다. GUI 재시작 없이 새 책을 다시 열 수 있어야 한다.
- crash 후 앱 임시물 정리는 다음 실행에 안전하게 수행한다. 정상 종료 잔류 0과 crash 복구를 구분한다.
