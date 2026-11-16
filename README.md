# nCatImage

**가볍게 열고, 빠르게 넘기는 Rust 기반 로컬 코믹뷰어.**

폴더와 ZIP/CBZ를 바로 열어 이미지를 감상하는 Linux·Windows용 뷰어입니다. 로컬 파일과 OS가 연결하는 SMB·FTP·SFTP 네트워크 폴더를 읽을 수 있습니다. 원본 파일을 변경하지 않으며, 자동 원격 분석이나 감상 기록 저장 없이 동작합니다.

> **v0.4.3 · 개발 프리뷰** — 아직 MVP/정식 안정판이 아닙니다. 지원 형식과 아래 제한 사항을 확인해 주세요.

**이 프로젝트는 OpenCode 코딩 에이전트(Coding Agent)를 통한 AI 기반 개발 프로젝트입니다.** 설계·구현·테스트·문서에 AI가 생성하거나 수정한 내용이 포함되어 있습니다. 자동 검사 통과가 안전성·정확성·성능을 보장하는 것은 아니며, 실제 검증 범위와 남은 제한을 공개합니다.

### v0.4.3 변경 사항

- SMB·FTP·SFTP URI를 KIOFuse 또는 GVfs/FUSE를 통해 연결합니다. 네트워크 버튼·`Ctrl+L`·드롭·대화상자·명령행에서 사용할 수 있습니다.
- 연결 대기를 비동기로 처리하고 이전 요청 취소·오래된 결과 거부·60초 대기 제한을 추가했습니다. 비밀번호 포함 URI는 거부하며 인증은 OS 도구에 맡깁니다.
- 드롭 형식 협상·수신·열기·표시와 네트워크 연결 결과를 경로·서버·계정 없이 기록합니다.
- FUSE 최초 읽기 중 속성 갱신에 따른 변경 오인을, 표시 전 새 열기 1회 재시도로 보완했습니다. 기존 리비전 검사는 유지합니다.
- 개발 문서·worker 통합 테스트·검증 도구·Linux CI를 저장소에 포함했습니다.

[전체 작업 내역](CHANGELOG.md) · [실제 구현·검증 결과](doc/08-implementation-status.md) · [검증 도구 실행법](tools/README.md)

## 다운로드 및 실행

### Linux

[**Linux x86_64 AppImage 다운로드**](https://github.com/NyarCat/nCatImage/releases/tag/v0.4.3)

GTK와 이미지 처리 worker를 포함한 **AppImage 파일 하나**로 배포합니다. Rust나 GTK 개발 패키지를 따로 설치할 필요가 없습니다.

```bash
chmod +x nCatImage-0.4.3-x86_64.AppImage
./nCatImage-0.4.3-x86_64.AppImage
./nCatImage-0.4.3-x86_64.AppImage /path/to/book.cbz
```

- 대상: **Linux x86_64, glibc 2.39 이상**(Ubuntu 24.04 및 동급 이상 환경).
- X11 또는 Wayland 그래픽 세션과 시스템 그래픽·폰트 환경이 필요합니다. macOS, ARM64, musl 기반 Alpine은 지원 대상이 아닙니다.
- 호환성을 위해 배포본은 기본적으로 Cairo 소프트웨어 렌더러를 사용합니다. `GSK_RENDERER`를 지정하면 변경할 수 있습니다.
- 모든 배포판·GPU·입력기 조합에서 검증된 것은 아닙니다. AppImage가 모든 Linux에서 실행된다는 뜻은 아닙니다.
- 외부 fcitx/IBus 모듈은 포함하지 않습니다. 시스템 입력기 설정에 따라 경고나 한글 입력 문제가 있을 수 있으며 별도 검증이 필요합니다.

FUSE 관련 오류가 나면 설치 없이 압축 해제 실행을 시도하세요:

```bash
./nCatImage-0.4.3-x86_64.AppImage --appimage-extract-and-run
```

다운로드 파일의 무결성은 릴리스에 함께 제공하는 `SHA256SUMS`로 확인할 수 있습니다:

```bash
sha256sum -c SHA256SUMS
```

### Windows

[릴리스 페이지](https://github.com/NyarCat/nCatImage/releases)에서 둘 중 하나를 받으세요. 두 파일 모두 GTK 런타임과 worker를 포함하므로 GTK나 Visual C++ 재배포 패키지를 따로 설치할 필요가 없습니다.

| 파일 | 설명 |
|---|---|
| `nCatImage-<버전>-windows-x86_64-setup.exe` | 설치 프로그램. 기본은 관리자 권한 없는 사용자별 설치입니다. 시작 메뉴 바로가기, 제거 프로그램, 이미지·ZIP/CBZ의 “연결 프로그램” 항목, 선택 항목인 폴더 우클릭 “nCatImage로 열기”를 추가합니다. 기존 기본 앱은 바꾸지 않습니다. |
| `nCatImage-<버전>-windows-x86_64.zip` | 포터블. 압축을 풀고 `nCatImage\nCatImage.exe`를 실행합니다. 폴더 안의 파일 구성을 그대로 유지해야 합니다. |

```powershell
nCatImage.exe C:\path\to\book.cbz
```

- 대상: **Windows 10/11 x64**.
- 코드 서명이 없어 처음 실행할 때 SmartScreen 경고가 나올 수 있습니다.
- GTK 기본 렌더러를 사용합니다. 그래픽 문제가 있으면 환경변수 `GSK_RENDERER=cairo`로 소프트웨어 렌더러를 쓸 수 있습니다.
- worker에는 Windows 작업 개체(job object)로 프로세스당 메모리 상한(1GiB), 하위 프로세스 생성 금지, GUI 종료 시 함께 종료가 적용됩니다. Linux와 마찬가지로 파일시스템·네트워크 샌드박스는 아닙니다.
- Windows 빌드는 Linux보다 검증 범위가 좁습니다.

무결성은 PowerShell의 `Get-FileHash <파일>` 결과를 `SHA256SUMS-windows`와 비교해 확인할 수 있습니다.

## 주요 기능

- 이미지 파일, 폴더, ZIP/CBZ 열기와 파일 드래그 앤 드롭
- OS 연결 도구를 통한 SMB·FTP·SFTP 파일/폴더 열기
- 자연 정렬, 이전/다음 페이지 및 페이지 번호 이동
- JPEG, PNG, BMP, WebP, GIF 첫 프레임 표시
- EXIF 방향 적용, 임시 회전, 맞춤, 채우기(비율 유지하며 창 크기까지 확대), 확대, 스크롤·드래그 패닝, 전체 화면
- 좌우 읽기 방향 전환
- 별도 worker 프로세스에서 이미지 처리, 오래된 요청 취소
- 바이트 제한 페이지 캐시와 인접 페이지 미리 읽기

### 현재 제한

RAR/CBR·7Z, 두 장 보기 UI, 이어보기, 썸네일, ICC 색관리, 애니메이션 재생은 아직 지원하지 않습니다. 폴더는 하위 폴더를 탐색하지 않습니다.

현재 출력은 **최대 8MiB 미리보기**이며 원본 해상도·4K 최종 화질을 보장하지 않습니다. 큰 이미지는 메모리 제한으로 거부될 수 있습니다. 기본 이미지 처리 예약 예산은 GUI와 worker 합산 192MiB이며, 앱 전체 RSS 제한과는 다릅니다.

## 단축키

페이지 정보와 조작 UI는 **뷰어 상단**에 있습니다. 일반 창에서는 백틱(<kbd>&#96;</kbd>)으로 상단 UI 전체를 숨기거나 표시합니다. 전체화면에서는 항상 숨기며, `F11`로 돌아오면 이전 표시 설정을 복원합니다.

| 동작 | 키 |
|---|---|
| 파일 / 폴더 열기 | `Ctrl+O` / `Ctrl+Shift+O` |
| 네트워크 주소 열기 | `Ctrl+L` 또는 상단 **네트워크** 버튼 |
| 다음 / 이전 페이지 | `↓`, `Space`, `PageDown` / `↑`, `Backspace`, `PageUp` |
| 읽기 방향에 따른 페이지 이동 | `←` / `→` |
| 읽기 방향 전환 | `D` |
| 확대 / 축소 / 맞춤 | `+` / `-` / `9` |
| 채우기 켜기 | `0` (맞춤 `9`로 해제) |
| 회전 / 반대 회전 | `R` / `Shift+R` |
| 다시 열기 | `F5` |
| 상단 UI 표시 / 숨김 | 백틱(<kbd>&#96;</kbd>), 일반 창에서만 |
| 전체 화면 전환 | `F11` |
| 종료 | `Esc` / `Ctrl+Q` |

페이지 번호 등 UI의 키보드 조작은 **상단 UI를 직접 클릭했을 때만** 활성화됩니다. 그 전에는 `Tab`으로 UI에 포커스가 옮겨가지 않으며, 페이지 단축키가 우선합니다. UI를 클릭한 뒤에는 입력 위젯의 기본 키 동작을 사용하고, 이미지 영역을 클릭하면 페이지 단축키 모드로 돌아갑니다. `↑`/`↓`는 좌우 읽기 방향과 관계없이 이전/다음 페이지로 이동합니다. 메인 뷰어의 `Esc`는 전체화면 해제가 아니라 **앱 종료**이며, 페이지 번호 입력 중에도 적용됩니다. 별도 파일 선택·네트워크 주소 입력 창에서의 `Esc`는 해당 대화상자를 취소합니다.

## 네트워크 폴더: Samba / FTP / SFTP

상단 **네트워크** 버튼이나 `Ctrl+L`에서 주소를 입력하거나, 파일 관리자에서 파일/폴더 **한 개**를 드롭하세요. 파일/폴더 열기 대화상자와 명령행의 네트워크 URI도 같은 연결 경로를 사용합니다.

```bash
./nCatImage-0.4.3-x86_64.AppImage 'smb://server/share/comics/'
./nCatImage-0.4.3-x86_64.AppImage 'ftp://server/comics/book.cbz'
./nCatImage-0.4.3-x86_64.AppImage 'sftp://user@server/comics/'
```

- **KDE:** 세션의 **KIOFuse**와 해당 프로토콜의 KIO worker를 사용해 로컬 접근 경로를 얻습니다. Debian/Ubuntu 계열은 `kio-fuse kio-extras`, Arch 계열은 `kio-fuse kio-extras` 패키지를 확인하세요.
- **GNOME/GVfs:** KIOFuse가 없으면 GIO의 원격 백엔드와 GVfs/FUSE 연결을 시도합니다. Debian/Ubuntu에서는 `gvfs-backends gvfs-fuse`가 필요합니다. AppImage와 시스템 GIO 모듈의 호환성에 따라 이 경로를 사용할 수 없을 수 있습니다.
- **OS에서 이미 마운트한 경로**(`/mnt/...`, CIFS, sshfs 등)는 기존 로컬 경로와 동일하게 읽습니다. 연결 도구가 없으면 앱에 표시되는 안내에 따라 OS에서 먼저 마운트하세요. 데스크톱 연결 도구는 AppImage에 포함하지 않습니다.
- 인증은 OS 연결 도구가 처리합니다. 먼저 파일 관리자에서 접속·로그인하면 편리합니다. 앱은 서버·계정·비밀번호를 설정 파일이나 로그에 저장하지 않으며, **비밀번호가 포함된 URL은 거부**합니다. OS 도구가 보관하는 자격증명은 해당 도구의 설정을 따릅니다.
- **FTP는 암호화되지 않습니다.** 민감한 자료에는 SFTP 또는 안전하게 설정한 SMB를 권장합니다. HTTP·WebDAV·FTPS 등 다른 URI는 이번 범위가 아닙니다.
- 연결 요청은 비동기로 처리하고 앱의 결과 대기를 최대 60초로 제한합니다. 다른 항목을 열거나 종료하면 이전 연결 결과가 화면을 덮어쓰지 않습니다. OS 도구 쪽의 연결·마운트 수명은 해당 도구의 정책을 따릅니다. 해석·디코딩·압축 읽기는 기존 worker에서 수행합니다.
- 앱에서 책 전체를 별도 다운로드하는 경로는 추가하지 않았습니다. 다만 KIO/GVfs가 탐색·랜덤 읽기를 위해 **네트워크 데이터나 파일 전체를 OS 캐시에 저장할 수 있으며**, 해당 데몬의 캐시는 앱의 192MiB 예약 예산에 포함되지 않습니다. 로컬 파일과 같은 속도를 보장하지 않으며, 느린 서버·연결 끊김에 따라 기존 worker 제한 시간에 걸릴 수 있습니다.

## 소스 빌드

### Linux

저장소의 `rust-toolchain.toml`에 지정된 Rust와 GTK **4.10 이상** 개발 패키지가 필요합니다. Ubuntu 24.04에서는 다음과 같이 빌드합니다:

```bash
sudo apt install build-essential pkg-config libgtk-4-dev
cargo build --workspace --release --locked
./target/release/ncat-linux /path/to/book.cbz
```

`ncat-linux`와 `ncat-worker`는 같은 디렉터리에 두세요. GUI만 빌드하면 worker가 누락될 수 있으므로 workspace 전체를 빌드해야 합니다.

AppImage 생성 방법과 필요한 추가 패키지는 [`packaging/linux/build-appimage.sh`](packaging/linux/build-appimage.sh)에 있습니다. 배포용 빌드는 최신 개발 머신이 아닌 Ubuntu 24.04 기준 환경에서 만드세요.

### Windows

Windows 10/11 x64에서 다음이 필요합니다.

- `rust-toolchain.toml`에 지정된 Rust(`x86_64-pc-windows-msvc`)
- Visual Studio 2022 이상 또는 Build Tools의 “C++를 사용한 데스크톱 개발”(MSVC, Windows SDK)
- [gvsbuild](https://github.com/wingtk/gvsbuild/releases)의 `GTK4_Gvsbuild_<버전>_x64.zip`을 `target\gtk`에 압축 해제. 다른 위치는 `GTK_DIR` 환경변수나 `-GtkDir`로 지정합니다. 2026.10.0(GTK 4.24.1)으로 검증했습니다.
- 설치 프로그램을 만들 때 Inno Setup 6(`winget install JRSoftware.InnoSetup`)

```powershell
powershell -ExecutionPolicy Bypass -File packaging\windows\build-windows.ps1
```

이 스크립트는 workspace를 빌드하고 GTK DLL·MSVC 런타임·리소스를 포함한 앱 폴더를 `target\windows\nCatImage`에 만듭니다. 이어서 Windows 기본 경로만으로 smoke test를 실행한 뒤 `dist\`에 포터블 ZIP, 설치 프로그램, `SHA256SUMS-windows`를 만듭니다. 설치 프로그램이 필요 없으면 `-SkipInstaller`를 붙이세요.

개발 중에 직접 빌드·실행하려면 pkg-config와 GTK `bin`을 지정합니다:

```powershell
$env:PKG_CONFIG = "$PWD\target\gtk\bin\pkg-config.exe"
$env:PKG_CONFIG_PATH = "$PWD\target\gtk\lib\pkgconfig"
$env:LIB = "$PWD\target\gtk\lib;$env:LIB"
$env:PATH = "$PWD\target\gtk\bin;$env:PATH"
cargo build --workspace --release --locked
.\target\release\ncat-linux.exe C:\path\to\book.cbz
```

## 실행 설정 및 오류 보고

| 환경변수 | 기본값 | 설명 |
|---|---:|---|
| `NCAT_MEMORY_MIB` | 192 | 이미지 관련 예약 예산, 1~384MiB |
| `NCAT_CACHE_MIB` | 총 예산의 1/8 | 캐시 상한, 0이면 비활성화, 최대 총 예산의 1/3 |
| `NCAT_WORKSPACE_MIB` | 128 | 작업 공간 상한, 8~384MiB, 총 예산으로 추가 제한 |
| `NCAT_TIMEOUT_SECS` | 15 | 작업 제한 시간, 1~120초 |
| `NCAT_PREFETCH` | 1 | 인접 페이지 미리 읽기, 0/1 또는 false/true |

시작 시 드롭 수신 준비 상태, 드래그 진입·형식 협상·수신 항목 수·열기 요청·표시 성공/실패 및 일부 worker 오류를 **실행한 현재 디렉터리의 `log/ncat-incident-*.log`**에 기록합니다. 드롭에는 로컬 또는 SMB·FTP·SFTP 파일/폴더 한 개를 사용할 수 있으며, 여러 항목이나 지원하지 않는 URI는 거부합니다. 파일 관리자의 파일 목록(`GdkFileList`/`text/uri-list`)과 단일 파일(`GFile`) 드롭을 지원합니다.

드롭 로그에는 단계·형식 지원 여부·X11/Wayland 구분·요청 세대·정제된 오류 코드만 남깁니다. 네트워크 연결도 `NetworkResolving` → `NetworkResolved` 또는 `NetworkFailed`/`NetworkCancelled`로 기록하며, 프로토콜 종류와 연결 도구만 구분합니다. 원본 경로·파일명·URI·서버·계정·비밀번호·이미지 내용은 기록하거나 외부 전송하지 않습니다. 사용자가 요청한 네트워크 읽기에는 해당 서버와 통신하며, 원격 분석 전송은 없습니다. 기록은 기존 coordinator에서 처리하므로 UI 스레드에서 디스크에 쓰지 않습니다. 대기 이벤트는 최대 32개, 로그는 최근 최대 8개이며, 저장 실패는 읽기를 중단하지 않습니다.

드롭이 계속 안 되면 다운로드한 AppImage가 있는 폴더에서 `./nCatImage-0.4.3-x86_64.AppImage`를 실행한 뒤 파일/폴더 한 개를 드롭하고, `log/`의 최신 로그를 첨부해 주세요. `Ready`만 있으면 드래그가 앱까지 도착하지 않은 경우이며, `Entered`/`Accepted` 뒤에 `Received`가 없으면 형식 협상·전달 단계, `OpenQueued` 뒤 `OpenFailed`이면 열기/디코딩 단계부터 조사할 수 있습니다. 로그 저장 위치를 찾기 어렵다면 앱을 실행한 터미널의 `pwd` 기준으로 확인하세요.

[이슈 등록](https://github.com/NyarCat/nCatImage/issues) 시 배포판, X11/Wayland, 앱 버전, 재현 방법과 관련 로그를 첨부해 주세요. 개인 이미지나 인증 정보는 올리지 마세요.

## 개발 상태

Linux·Windows x86_64용으로 개발 중인 개인 프로젝트입니다. 정식 코덱 호환성·보안·성능 검증이 완료된 제품이 아니며, 다른 뷰어보다 가볍다는 비교 결과를 주장하지 않습니다. 코드 라이선스는 아직 미정입니다. 배포본에 포함된 외부 구성요소의 라이선스 고지는 AppImage 내부 `usr/share/doc/ncatimage/`와 Windows 배포본의 `doc\third-party\`에 있습니다.
