# nCatImage 개발 지침

기준: v0.5.5 · 2026-11-19. OpenCode 코딩 에이전트를 통한 AI 기반 개발이며, 실제 실행한 검사와 미검증 범위를 구분해 보고한다. 사용자가 모든 개발 문서·테스트 코드 공개를 요청했으며, 개인정보·원본 입력·진단 로그·빌드 산출물은 소스 커밋에 넣지 않는다.

## 목적과 범위

- 제품은 가벼운 Rust 코믹뷰어다. 종합 사진 도구·편집기·라이브러리 관리자로 확장하지 않는다.
- 종전 ImageViewer/WPF 이관 설계는 폐기했다. 현재 사용자 요청에 따라 Windows/macOS 포팅을 준비하는 공통 presentation·GTK frontend·OS 서비스 경계를 분리한다. 종전 코드 재사용/라이선스 추정은 가져오지 않는다.
- 사용자 구현 요청에 따라 Cargo workspace와 Linux M1 개발판을 추가했다. 현재 범위는 [구현 상태](doc/08-implementation-status.md)가 기준이며 M0 전체/MVP 완료로 보고하지 않는다.
- 개발/실행 검증은 Linux x64 우선이다. Windows/macOS 포팅·배포 파이프라인을 준비하되 native 파일/보호 미구현 OS는 readiness gate에서 차단한다. 실제 세 OS 배포 완료로 표시하지 않는다. Android는 UX 참고 대상이다.
- Windows의 최종 목표는 DLL 추출 없는 단일 EXE + 동일 EXE의 별도 worker다. Win32 셸과 격리된 Wine prefix로 단계적으로 개발하며 [11](doc/11-windows-single-exe-wine.md)을 따른다. native 보호 미완료 때 decoder fallback을 허용하지 않는다.
- Windows 로컬 빌드는 WSL에 설치된 Rust와 Linux용 llvm-mingw로 교차 빌드한다. Cargo 중간 산출물은 WSL 파일시스템에 두고, 검증된 EXE는 저장소의 `target/windows-single/`에 둔다. native Windows 실행 검증과는 별개다.
- v0.5.2는 사용자 승인으로 native Windows **미시험 UNTESTED 실험판**을 별도 release asset에 포함한다. 정식 Windows readiness는 false로 유지하며, 실험판 공개를 native 검증 통과로 계산하지 않는다.
- 이 파일명은 `AGENT.md`다. `AGENTS.md`만 읽는 도구는 명시적으로 이 문서를 읽도록 설정한다.

## 읽을 문서

1. [문서 지도](doc/README.md)
2. [제품·우선순위](doc/01-product.md)
3. [코믹 읽기 UX](doc/02-reader-ux.md)
4. [Rust 구조·작업 수명](doc/03-architecture.md)
5. [형식·압축 지원](doc/04-formats-archives.md)
6. [메모리·반응성 정책](doc/05-memory-performance.md)
7. [보안·기록·배포](doc/06-security-data.md)
8. [검증·로드맵](doc/07-validation-roadmap.md)
9. [실제 구현·검증 기록](doc/08-implementation-status.md)

## 기술 원칙

- 탐색·정렬·보기 계산·페이지 구성·캐시·예산·설정/기록·코덱/압축 조정은 Rust다. Linux GUI도 gtk-rs/Rust를 우선한다.
- 외부 네이티브 코덱은 검증된 Rust 어댑터로 사용 가능하다. Rust 기반과 전이 의존성까지 순수 Rust인 구성을 혼동하지 않는다.
- GTK4 기본 위젯/표시 경로부터 시작한다. libadwaita·Tokio·Rayon·custom GPU renderer·DB·범용 플러그인·외국어 GUI ABI는 필요와 측정 근거 없이 추가하지 않는다.
- UI 스레드에서 파일 읽기·압축 해제·메타 파싱·디코딩·정렬·큰 복사·작업 대기를 하지 않는다. 새 이벤트가 GLib main context를 깨우게 하고 상시 polling/무한 redraw를 금지한다.
- Core는 unsafe를 금지한다. FFI/공유 메모리/OS 호출은 좁은 경계에서 안전 조건·owner·thread 제약을 기록한다.
- 비신뢰 코덱/압축 파싱은 별도 worker. 초기에는 **GUI+지연 시작 worker 1개**로 측정하고 무조건 worker/thread를 늘리지 않는다.
- 취소·최신 선택·소스 리비전·캐시 삽입·texture 업로드를 연결한다. 오래된 결과를 화면/캐시에 되살리지 않는다. 공유 작업은 한 요청자의 취소로 다른 구독자를 중단하지 않는다.
- 기본 이미지 관련 예산은 **192MiB/앱 프로세스의 GUI+모든 worker 합산**이다. 현재 표시와 다음 전경 workspace가 캐시보다 우선한다. GUI/driver 전체 RSS와 다르다.
- 프레임 수가 아닌 실제 bytes로 캐시·진행 작업을 제한한다. 입력 크기 상한은 표시 보장이 아니다. 저메모리에서 선행 읽기/썸네일/원본 캐시를 먼저 줄인다.
- Arc/RAII로 lease·예산 수명을 연결한다. texture가 참조 중인 픽셀을 먼저 반환하지 않는다. 종료는 UI 비차단 상태 전이이며 Drop에서 join하지 않는다.
- 편집·필터·변환·범용 파일 삭제/이동·주석·사진 DB·원격 서버 기능을 추가하지 않는다. 사용자 요청으로 현재 일반 이미지 한 장을 `Del`로 OS 휴지통에 보내는 조작만 예외다. 화면 회전/펼침 분할은 감상 변환이며 원본 불변이다.
- 원본 경로·압축 항목·선택 페이지·합성 canvas·실제 읽은 bytes를 검사한다. GUI 이미지 자동 로더·전체 프레임 collect·전체 아카이브 선행 해제로 우회하지 않는다.
- 로컬 읽기는 오프라인이며 원격 분석은 없다. 사용자 요청의 SMB/FTP/SFTP 읽기는 OS의 KIOFuse 또는 GVfs/FUSE에 맡긴다. 로그에 경로/원본/EXIF/서버/계정/비밀번호를 남기지 않는다. 감상 기록은 현재 저장하지 않으며 향후 opt-in 설계를 따른다.
- 문서/테스트/기능표를 함께 갱신한다. 성능 목표를 조용히 낮추거나 실패한 포맷을 지원으로 표시하지 않는다.

## 검증과 보고

- Linux headless 테스트를 실제 Wayland/X11·GPU·IME·접근성·패키지 검증으로 대신하지 않는다.
- 작업 보고에 실행한 검사·미검증·목표/실측 차이를 적는다. 다른 앱보다 가볍다는 주장은 동일 Linux 조건 비교 후에만 한다.
- 과거 설계 문서 폐기는 당시 승인 범위였으며 현재 문서 삭제 허가가 아니다. 사용자가 추가한 코드/문서/파일은 별도 허가 없이 제거하지 않는다.
- 변경 후 `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`를 실행한다. GUI 실행 전 `cargo build --workspace --release --locked`로 worker도 함께 빌드한다. Linux M0 미검증 항목과 미구현 기능을 상태 문서에서 추적한다.
