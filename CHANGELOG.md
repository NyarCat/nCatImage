# nCatImage 작업 내역

이 문서는 실제 반영한 작업을 기록한다. 계획·목표와 검증 범위는 [개발 문서](doc/README.md), 현재 제한은 [구현 상태](doc/08-implementation-status.md)를 따른다. OpenCode 코딩 에이전트를 통한 AI 기반 개발이며, AI 생성·수정 코드와 문서가 포함된다.

## 0.6.3 — 2026-11-27 · 페이지 번호 이동과 이미지 휴지통

- GTK 페이지 번호 칸에서 중복 `+`·`−`를 제거하고, GTK·Win32에서 `Enter`와 `▾`가 같은 번호 이동을 실행한다.
- `ⓘ` 버튼은 상단 UI와 같이 숨겨지고 표시된다. 이미 열린 메타데이터 패널과 `Tab` 토글은 유지한다.
- `Del`로 현재 일반 이미지를 확인 후 휴지통으로 보낸다. 확인창 생략은 현재 실행 중에만 유지한다. ZIP/CBZ 내부 이미지는 경고만 표시하고 수정하지 않는다.

## 0.6.2 — 2026-11-26 · 원본 배율과 창 크기

- 현재 배율과 잘리기 전 표시 크기를 오른쪽 아래 확대·축소 버튼 왼쪽에 표시한다. 메타데이터 패널의 겹친 문서 아이콘으로 일반 텍스트를 복사한다.
- 시작할 때만 첫 이미지에 맞춰 창을 필요한 방향으로 확장한다. 원본 배율에서 여유가 있는 방향으로 가운데 정렬하고, 창을 축소하면 넘치는 오른쪽·아래를 가린다. 종료 시 마지막 일반 창의 크기를 저장한다.
- Linux는 Wayland 분수 배율을 사용한다. Windows는 Per-Monitor DPI Awareness v2로 OS의 창 비트맵 확대를 막고 모니터 변경 시 새 창 영역을 적용한다. 정식 Windows readiness와 기존 워커 보호 정책은 바꾸지 않았다.
- Linux x64 AppImage와 미서명 Windows x64 `UNTESTED` 단일 EXE를 개발 프리뷰로 배포한다.

## 0.6.1 — 2026-11-26 · 앱에서 로그 보기

- Linux·Windows 우클릭 메뉴에 읽기 전용 **로그 보기**를 추가했다. 최근 128줄만 메모리에 보관하며 진단 로그는 파일로 저장하지 않는다. 이전에 저장된 로그는 변경하지 않는다.

## 0.6.0 — 2026-11-19 · 한 장씩 페이지 이동과 서리 유리 UI

- Linux·Windows에서 새 페이지를 읽는 동안 기존 이미지를 유지하고 추가 이동 입력을 무시한다. 실패하면 오류를 표시하고 다음 입력으로 이동을 재개한다.
- Linux 회귀 테스트를 새 창 제목·오버레이 고정 방식에 맞췄고, 손상 페이지를 만난 뒤 다시 이동하는 검사도 추가했다. Breeze 스크롤바의 음수 크기 경고를 수정했다.
- Linux AppImage와 미서명 Windows `UNTESTED` 단일 EXE 프리뷰. Windows 네이티브 확인 및 정식 배포 검사는 완료되지 않았다.

- Linux·Windows에서 `[`/`]`로 같은 부모 폴더의 이전·다음 책을 연다. 지원 이미지가 바로 들어 있는 폴더와 ZIP/CBZ를 자연 정렬하며, 처음과 끝은 순환하지 않는다.

- ImageViewer에서 착안한 플로팅 UI를 서리 유리 스타일로 다듬었다. 패널 뒤의 뷰어 이미지만 흐리고, 버튼과 글자는 선명하게 유지한다. 새 외부 라이브러리는 추가하지 않았다.
- 페이지 조작은 상단 중앙, 애니메이션 조작은 왼쪽 아래, 확대·축소는 오른쪽 아래로 분리했다. 파일·폴더·주소 열기는 우클릭 메뉴로 옮겼으며 주소는 별도 입력 창에서 받는다. 상태와 현재/전체 프레임은 창 제목에 표시하고 재생 상태는 `▶`/`Ⅱ` 버튼으로 구분한다.
- 오른쪽 위 `ⓘ`와 기존 `Tab`/`Shift+Tab`으로 오른쪽 메타데이터 패널을 토글한다. 창 크기를 조절하는 중에도 열린 패널을 유지한다.
- Windows의 실제 마우스 드래그에서 그리기 요청이 막혀 이전 버튼이 계단식으로 남던 문제를 수정했다. 버튼을 표시한 채 연속 크기를 바꾸는 경우와 실제 마우스 드래그를 별도로 검사했다.
- 실제 데스크톱의 [렌더링·배율·접근성 등 미검증 항목](doc/14-reader-control-layout.md#linux-릴리스-전-확인)은 프리뷰에서 정식 검증으로 계산하지 않는다.

## 0.5.5 — 2026-11-19 · 플로팅 바와 Windows 창 크기 변경

- ImageViewer에서 착안한 상·하단 플로팅 바와 개별 고정·가장자리 자동 표시를 GTK4와 Win32에 독립 구현했다. 바가 표시되거나 숨겨져도 이미지 배율은 바뀌지 않는다. Tab 메타데이터와 기존 페이지·프레임 조작은 유지한다.
- Windows 크기 변경 시 이전 화면 픽셀을 그대로 복사하지 않고, 바의 컨트롤을 한 번에 배치한 뒤 이미지·바·컨트롤을 다시 그린다. 연속 크기 변경과 최대화·복원 시 컨트롤 좌표·표시 상태를 확인하는 회귀 검사를 추가했다.
- 확대·축소 중에는 기존 프레임을 유지하고 조작이 멈춘 뒤 새 디코딩을 요청한다. 중첩 대화상자가 완료 알림을 가져가도 디코딩 결과를 적용한다.
- 이번 프리릴리스는 Windows x64만 제공한다. Linux판은 UI 소스를 수정했지만 이번 빌드 및 검증 대상이 아니다.

## 0.5.4 — 2026-11-19 · Tab 메타데이터 floating layer

- main viewer의 Tab/Shift+Tab 컨트롤 포커스 이동을 제거하고 메타데이터 레이어 토글로 바꿨다. 기본 off이며 이미지에 embedded metadata가 있을 때만 이미지 영역 왼쪽 위에 표시한다. GTK/Win32에 적용하며 modal OS 대화상자의 표준 입력은 유지한다.
- worker에서 PNG tEXt/zTXt/iTXt/eXIf, JPEG EXIF/XMP/Comment, WebP EXIF/XMP, GIF Comment를 bounded 추출한다. 렌더된 이미지·ZIP/CBZ·애니메이션 next/seek/cache에도 연결하며 원본을 수정하거나 metadata를 로그/디스크에 별도 저장하지 않는다.
- 표시 텍스트는 최대 32항목·총 32KiB, 값당 4096bytes로 제한하며 제어/방향 제어 문자를 제거한다. EXIF/XMP/text를 markup으로 실행하지 않는다. 입력 스캔/압축 해제 상한·표시 복사 예약·부모 wire 검증을 추가했다. binary ICC 등 모든 metadata 형식을 표시한다는 의미는 아니다.
- wire v5로 변경했다. GUI/worker는 같은 빌드를 사용해야 한다. 기본 overlay 상태는 프로세스마다 off이며 별도 설정 저장은 없다. v0.5.4로 공개하고 기존 v0.5.3 파일은 교체하지 않는다.
- Linux Rust 97개(장기 1개 ignored)·GTK metadata 3개·기존 animation/control 8개·Python 배포 7개, Windows-target Wine unit 36개(native symlink 1개 ignored)·UI/platform 5개 통과. 로컬 전체 desktop 회귀는 xvfb-run 부재로 실행하지 못했으며 패키지 CI에서 검증한다.
## 0.5.3 — 2026-11-17 · Windows 크기 변경 재그리기

- 드래그 종료에 한정됐던 재렌더 요청을 크기 변경·최대화/복원·전체 화면·UI 숨김/표시에 연결했다. 동기 메시지 재진입에서 `RefCell` borrow 실패로 갱신을 놓치지 않도록 별도 메시지로 처리한다.
- 기존 프레임은 즉시 다시 그리고, 변경이 100ms 동안 안정되면 새 viewport로 backend에 요청한다. 동일 크기·최소화·종료 때는 불필요한 요청을 피한다.
- `--smoke-resize`는 실제 창의 크기 변경·최대화/복원·전체 화면·UI 전환·최소화/복원 뒤 viewport 갱신과 paint 발생을 검사한다. 빈 창/Wine 검사이며 protected 이미지 표시 성공이나 native Windows 실사용 검증을 의미하지 않는다. v0.5.2 공개 파일은 교체하지 않았다.
- Windows는 계속 UNTESTED 실험판이다. v0.5.2 native CI에서 확인된 DACL 오류는 이번 크기 변경 수정으로 해결되지 않았으며 보호를 완화하지 않는다.

## 0.5.2 — 2026-11-17

- **Windows x86_64 실험판 포함: 실제 Windows에서 테스트하지 않았다.** 정상적인 이미지 표시·보안·입력 동작을 보장하지 않으며 Wine에서는 Job 제한 검증 실패로 protected decode가 차단된다. 정식 readiness는 false다.
- [개발 계획](doc/11-windows-single-exe-wine.md), GTK 비의존 Win32 뷰어·단일 EXE PE import 검사·private Wine prefix 도구와 개발 CI를 추가했다.
- Windows UTF-16LE 경로·invalid wire 경로 거부, 공통 worker executable/argument 경계, 동일 EXE 재실행을 위한 worker library entry를 준비했다.
- Windows file ID/private DACL/final reparse 검사·strict Job handshake·같은 EXE worker를 구현하고 탐색/보기/애니메이션 UI를 연결했다. native 보안·부모 crash·DPI/접근성·FTP/SFTP는 미검증/미완료다.
- Linux Rust 90개(장기 1개 ignored), GTK 8개, Python 7개, Windows-target Wine unit 30개(native symlink 1개 ignored) 및 platform/빈 창/worker 거부 3개 통과. Windows native 검증과 구분한다.

## 0.5.1 — 2026-11-16

- 디코딩 busy/프레임 경계마다 이전·다음 버튼을 비활성화하던 처리를 제거했다. 자동 재생 중 page navigation/title 갱신도 반복하지 않는다.
- 프레임/전체 수·상태·배속 표시를 타이틀바에서 애니메이션 툴바의 동일 행으로 옮겼다.
- 일시정지 중 1-based 프레임 번호·이동 버튼/Enter를 추가했다. 아직 전체 수를 모르는 입력의 범위 초과는 재생 wrap/프로토콜 오류 대신 마지막 유효 프레임으로 clamp한다.
- wire v3의 bounded total metadata와 프레임 이동/버튼 상태 회귀를 추가했다. GUI/worker는 같은 빌드로 배포한다.
- Rust 테스트 82개(장기 1개 ignored), 실제 GTK 재생/컨트롤 검사 8개·fmt·Clippy·release 빌드 통과. AppImage는 CI의 패키지 검사를 통과한 artifact로 제공하며 Windows/macOS 차단은 유지한다.

## 0.5.0 — 2026-11-16

Linux x86_64 AppImage 우선 배포. Windows/macOS는 native 파일/worker 보호 미구현으로 차단되며 ZIP/DMG 배포본이 없다. 기존 v0.4.3 Release/태그는 유지한다.

### 애니메이션 재생

- 타이틀바의 현재/전체 프레임·상태·배속 표시, 이전(`,`)/다음(`.`)/재생·일시정지(`/`) 버튼·단축키와 0.25×~4× 속도 선택을 추가했다. 전체 프레임 수는 첫 주기 완료 뒤 확인한다.
- 이전 프레임은 bounded worker seek로 다시 합성한다. finite 재생 종료 뒤에도 예약된 세션을 유지해 이전 이동/재시작을 지원하고 새 페이지·취소·종료에서 해제한다. 모든 decoded frame을 저장하지 않는다.

- GIF/AGIF·WebP·APNG의 합성 프레임 순차 재생, 지연·유한/무한 반복을 연결했다. `.png` APNG와 ZIP/CBZ 내부 입력도 같은 worker 경로를 사용한다.
- persistent decoder/compositing workspace를 parent 예산에서 재생 수명 동안 예약하고, 애니메이션의 정지 cache 삽입·인접 prefetch를 억제한다. 이동/보기 변경·취소/종료는 이전 session을 해제한다.
- wire v2에 다음 프레임/재생 종료·유한 timing metadata를 추가했다. GUI와 worker는 같은 소스 버전으로 함께 빌드해야 한다.
- 합성 GIF/APNG disposal-previous, WebP 부분 프레임/background disposal, 반복·지연·예산/lease·원본 변경·ZIP/회전 회귀와 실제 GTK 7개 입력 재생 smoke를 추가했다. 전체 실물 형식·정확한 compositor FPS·일시정지/배속 UI 인증은 아니다.

- OS별 Distribution packages workflow를 추가했다: Linux AppImage, Windows GTK/worker 동봉 포터블 ZIP, macOS arm64 `.app`/DMG. Windows 단일 EXE는 구현하지 않고 ZIP fallback을 선택했다.
- native 포팅 readiness·태그/버전 검사·패키지 relocation smoke·체크섬·전 OS 성공 후 초안 릴리스 정책을 추가했다. 현재 Windows/macOS는 미구현 보호/파일 API 때문에 차단된다.
- `ncat-desktop` 로컬 GTK 진입점과 배포 문서를 추가했다. 기존 CI의 Openbox 시작 경합은 WM 준비를 기다리도록 보완했다.

- `ncat-presentation`에 툴킷 독립 읽기 명령·키 우선순위·페이지 이동·확대/회전·보기 크기·UI 표시 정책을 추출했다.
- GTK frontend를 `ncat-gtk`로 옮기고 Linux 진입점은 네트워크 연결 서비스를 주입한다. KIOFuse/GVfs 구현은 `ncat-linux`에 남긴다.
- 중복 파일 리비전과 private 파일·no-follow 출력·Linux worker 보호를 `ncat-platform`으로 분리했다. Unix 경로 인코딩은 protocol의 별도 모듈에 격리했다.
- 공통 로직 OS별 CI 정의와 경계 테스트, [포팅 문서](doc/09-portability.md)를 추가했다. Windows/macOS 앱 구현/실행 검증 완료는 아니다.

## 0.4.3 — 2026-11-16

### 네트워크 폴더

- `smb://`, `ftp://`, `sftp://`를 OS 연결 도구의 로컬 경로로 해석한다. KIOFuse D-Bus 연동을 우선하고, 사용 불가 시 GIO 원격 백엔드와 GVfs/FUSE를 시도한다.
- 네트워크 버튼·Ctrl+L 주소 입력, 드롭, 파일/폴더 대화상자, 명령행을 공통 열기 경로로 연결했다.
- 새 열기·페이지 선택·종료 시 이전 연결 future를 취소하고 요청 번호로 늦은 응답을 무시한다. 앱 결과 대기는 최대 60초이며 OS 마운트 자체의 수명은 OS 도구 정책을 따른다.
- 프로토콜·URI 형식·호스트·16KiB 길이·응답 경로를 검사한다. 비밀번호 포함 URI는 거부한다. 계정 관리·영구 URI 저장·자체 다운로드 캐시는 추가하지 않았다.
- 실제 FTP 폴더/CBZ의 500회 탐색을 확인했다. SMB/SFTP는 모의 연결 도구로 요청 전달·표시·취소를 확인했으며 실제 서버 인증·GNOME/GVfs 전체 경로는 별도 미검증이다.

### 드롭 및 진단

- `GdkFileList`/URI 목록을 우선하고 단일 GFile도 받는다. 한 항목만 허용하며 자식 입력 위젯보다 먼저 드롭을 처리한다.
- X11 표준 URI 드롭은 기존 빌드에서도 성공했다. 사용자 Wayland 로그에서는 네트워크 항목이 수신된 뒤 `NonLocal`로 거부되었고, 포털이 로컬 경로를 준 드롭은 표시에 성공한 것을 확인했다. 모든 드롭 실패가 형식 문제라고 단정하지 않는다.
- 시작 준비, 진입, 형식 협상, 항목 수, 거부, 열기, 표시, 네트워크 연결·취소·실패를 정제된 타입으로 기록한다.
- GUI 진단 큐는 최대 32개이며 기존 coordinator가 디스크에 쓴다. 직전 128개 상태, 줄당 최대 1KiB, 최근 관리 로그 최대 8개, 파일 권한 0600을 유지한다. 경로·파일명·URI·서버·자격증명은 기록하지 않는다.
- FUSE 첫 읽기 중 stat 속성 갱신으로 발생한 Changed를 실제 FTP에서 재현했다. catalog/frame 게시 전 최초 열기를 새 worker로 한 번 재시도하며 기존 리비전 검사는 제거하지 않았다.

### 문서·검증·배포

- 모든 개발 Markdown, worker 통합 테스트, backend 예제, 합성 입력·메모리·GUI·네트워크 검증 도구와 Linux CI를 공개한다. 사용자 ZIP·개인 이미지·로그·실행 바이너리는 소스 커밋에서 제외한다.
- README에 AI 코딩 에이전트 기반 개발 사실, 변경 내역, 사용법·보안·미검증 범위를 명시했다.
- Linux x86_64 AppImage를 Ubuntu 24.04 기준으로 빌드한다. GTK와 worker를 포함하며 glibc 2.39 이상, 기본 Cairo renderer, 체크섬·고지를 제공한다. 개발 프리뷰이며 코드 서명·완전 static·모든 배포판 지원은 보장하지 않는다.
- 기본 Rust 테스트 56개 및 수동 장기 시험 1개 ignored. 포맷·Clippy·회귀 실행 결과와 재현 명령은 08 및 tools 문서에 기록한다.

## 0.4.2 — 2026-11-15

- 앱 이름을 저장소와 같은 nCatImage로 통일했다. 창 제목·오류 메시지·앱 ID(`io.github.ncatimage.Development`)·데스크톱/아이콘·AppImage 파일명·내부 고지 경로를 변경했다.
- README 사용법과 Linux AppImage 배포를 갱신했다. 당시 드롭·네트워크 보강은 아직 릴리스에 포함되지 않았다.

## 0.4.1 — 2026-11-15

- 페이지 정보와 조작부를 하단에서 상단으로 이동했다.
- 일반 창에서 백틱으로 상단 전체를 토글하고 전체화면에서는 숨긴다. F11 복귀 시 이전 표시 설정을 복원한다.
- 상단 UI를 클릭해야 컨트롤 키보드 조작을 활성화하며 그 전 Tab 포커스 진입을 막는다. 이미지 클릭 후 읽기 단축키로 돌아간다.
- ↑/↓를 이전/다음 페이지로 매핑하고 메인 뷰어 Esc를 종료로 변경했다. 별도 대화상자의 Esc는 취소다.
- 키보드·실제 마우스 입력·전체화면 회귀와 AppImage를 확인했다.

## 0.4.0 — 2026-11-15

- Rust 6개 crate workspace, Linux GTK 단일 페이지 UI, 별도 worker의 파일/폴더/ZIP/CBZ 열기, 자연 정렬·기본 정지 이미지·맞춤/확대/회전/패닝을 연결했다.
- 192MiB 예약 예산·마지막 owner까지 유지되는 픽셀 lease, byte 제한 LRU·인접 페이지 prefetch·세대별 결과 무효화·유한 worker 복구와 진단을 구현했다.
- 사용자 대용량 ZIP의 빠른 이동 오류를 조사했으나 원래 오류의 정확한 원인을 재현·확정하지 못했다. 변경되지 않은 소스의 모순된 worker 응답에 한해 복구를 보강했다.
- README·패키징 도구와 첫 Linux AppImage 개발 프리뷰를 배포했다. JPEG 축소 디코딩·ICC·RAR·두 장 UI·이어보기·정식 성능/접근성 게이트는 미완료다.
