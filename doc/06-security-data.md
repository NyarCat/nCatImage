# 06. 안전한 입력·감상 기록·패키지

아래는 목표 보호/배포 계약이다. 현재 M1의 구현된 제한과 미완료 sandbox·색/metadata 검사·기록/배포 항목은 [08](08-implementation-status.md)에 구분한다. 개발판을 안전성 인증 제품으로 취급하지 않는다.

v0.4.3 · 2026-11-16. AI 코딩 에이전트 기반 개발이며, 고지 수집이나 자동 검사 통과를 법률·보안 검토 완료로 표현하지 않는다.

## 1. 비신뢰 경계

v0.5.4 metadata 레이어도 같은 경계를 사용한다. optional EXIF/text/XMP는 worker에서 bounded 추출하고 wire v5 parent 검증 후 plain text로만 표시한다. 제어/bidi 제거·32항목/32KiB·기본 off·로그/원본 불변·표시 복사 예약은 [12](12-metadata-overlay.md)에 명시한다. 모든 vendor metadata/ICC 검사나 native Windows 보호 검증 완료를 뜻하지 않는다.

로컬 이미지라고 안전하지 않다. 파일/압축 header·항목명·EXIF/ICC·TIFF 내부 페이지·첫 합성 frame·worker 출력까지 비신뢰다. Rust 메모리 안전성만으로 압축 폭탄·무한 연산·native FFI·디스크 고갈을 막았다고 보지 않는다.

- parser·실제 포맷 식별·압축 index·codec·metadata는 worker에서 수행한다. GUI의 기본 file image loader나 전체 아카이브 추출로 우회하지 않는다.
- 원본은 읽기 전용으로 열고 회전/펼침/맞춤으로 저장하지 않는다. no-edit는 UI에서 버튼을 숨기는 것뿐 아니라 writer/encoder/원본 변경 경로를 만들지 않는 것이다.
- native FFI는 allocator/stride/alignment/thread/lifetime 계약을 검사하고 좁게 감싼다. panic catch는 abort/OOM/native 손상 복구 수단이 아니다.
- 임의 delegate·스크립트·shell 명령 실행·자동 codec 다운로드는 금지한다. 사용자 요청의 SMB/FTP/SFTP 읽기만 OS의 KIOFuse D-Bus 또는 GIO/GVfs/FUSE로 연결한다. 업데이트 검사·원격 분석·이미지 업로드는 없다.
- URI는 16KiB 한도·허용 scheme·호스트·형식을 검사하고 비밀번호 포함 URI를 거부한다. OS 인증·SSH 호스트 키·자격증명 보관 정책은 OS 도구가 소유한다. FTP는 암호화되지 않는다. 앱에 서버/계정/URI 저장소를 만들지 않는다.

## 2. 초기 입력 제한

| 자원 | 초기 한도 | 검사 |
|---|---:|---|
| 실제 선택 page/canvas | 150MP | header와 실제 선택 frame/page, 05 예산이 추가로 우선 |
| 단일 이미지/항목 읽기 | 512MiB | 실제 읽은 bytes, RAM 전체 적재 금지 |
| 책의 후보 항목 | 100,000 | 순차 index 구축 중 검사·초과 안내 |
| 항목명 | 원시 16KiB/개 | 제어/NUL·방향 문자 정제 전 제한 |
| 전체 index/meta | 소스당 24MiB | parent/worker 중복과 표시 문자열 포함, 05 예산 안에서 예약 |
| 이미지 EXIF/ICC/주석 | 작업당 8MiB | 할당 전 길이·parser 내부 선행 할당도 확인 |
| IPC 제어 메시지 | 256KiB/개 | 역직렬화 전 길이, 큰 목록/meta는 유한 배치 |
| 요청당 압축 스캔/해제 | 누적 4GiB | 선택 출력뿐 아니라 건너뛴 solid 블록 처리 bytes도 추적 |
| 단일 작업 | 기본 15초 | 실제 timeout·kill/reap, 진행 신호 무한 연장 금지 |
| 임시 디스크 | 프로세스 합산 512MiB | spool/solid cache 함께 포함·디스크 부족 시 중단 |

수치는 07의 PoC에서 기본 만화·큰 책·악성 경계를 함께 보고 확정한다. 100,000항목보다 먼저 index 예산에 닿을 수 있으며 둘 중 먼저 도달한 상한을 따른다. parser가 앱 검사 전에 할당하는 경우 native/library 제한·worker hard limit으로 보완한다. “모든 대형 책 지원”은 약속하지 않는다.

- 바이트/차원/시간은 checked 산술로 계산한다. header의 크기·확장자·압축률만으로 신뢰하지 않는다.
- 암호화/분할 압축·중첩 자동 탐색은 거부한다. archive path를 OS 추출 경로로 사용하지 않고 entry ID로 읽는다. symlink/hardlink/device/sparse 특수 항목은 거부한다.
- 파일 경로는 `PathBuf/OsString`, Unix 비UTF-8 원시 bytes와 UI 표시 문자열을 분리한다. IPC/기록에 tagged lossless 표현을 쓰고 lossy/case-fold 이름으로 파일을 다시 열지 않는다.
- 원본 변경/삭제/재열기는 리비전과 BookEpoch를 갱신한다. 크기+mtime만으로 내용 동일성을 확정하지 않는다. P0 수동 새로고침과 읽기 실패 감지, 필요 시 P1 감시 hint를 사용한다.

## 3. worker 보호와 temp

- Linux에서 rlimit/user cgroup·process group/parent death·kill/reap·fd 상속을 PoC한다. 192MiB 앱 예산은 OS RSS hard limit이 아니며 mapping/native overhead를 보고 hard limit을 별도로 확정한다.
- 가능한 입력 handle·앱 temp만 전달하고 네트워크/새 프로세스/추가 파일 접근 제한의 실효성을 시험한다. namespace/seccomp는 지원/권한/codec 영향 검증 후 적용한다.
- 별도 프로세스를 완전한 sandbox로 홍보하지 않는다. 보호 기능 적용 여부를 capability로 기록하고 실패를 숨기지 않는다. 미해결 고위험 항목은 출시 차단 또는 범위 재승인 사유다.
- temp는 앱 소유 private directory·난수 파일명·사용자 전용 권한·안전한 open/no-follow를 사용한다. 원시 entry 이름을 붙이지 않는다.
- 읽기/취소/오류/종료에서 temp 예약과 handle을 회수한다. crash 잔여물은 다음 실행에서 소유/활성 세션 확인 후 정리하며 다른 실행/원본 파일을 삭제하지 않는다.
- 공유 픽셀은 게시 후 worker 변경을 막거나 부모 소유로 복사한다. parent가 dimensions/stride/length/ID를 재검사하고 마지막 GUI owner까지 lease를 유지한다.

## 4. 설정과 이어보기

- Rust backend가 XDG_CONFIG_HOME/STATE_HOME/CACHE_HOME 표준 경로에 설정/기록/캐시를 분리한다. 미설정 시 표준 fallback을 사용한다. SQLite/사진 DB는 도입하지 않는다.
- 설정은 작은 versioned JSON, 감상 기록은 별도 파일이다. 최대 최근 책 100개를 초기 한도로 둔다. book별 마지막 페이지/반쪽·anchor·읽기 모드·선택 북마크만 저장하며 이미지/전체 metadata는 저장하지 않는다.
- 기록은 기본 꺼짐, 최초 읽기 후 한 번 opt-in 안내다. “기록 없이 열기”는 기존 기록의 갱신도 하지 않는다. 개별/전체 삭제와 설정/로그/temp 삭제를 구분한다.
- last normal frame 표시 후 debounce 1초/책 닫기/종료 시 유한 flush를 수행한다. crash 직전 1초의 기록 손실 가능성을 안내하고 매 input마다 fsync하지 않는다.
- source ID/컨테이너 revision·원시 entry/내부 page·반쪽을 검증해 복원한다. entry index만 믿지 않는다. revision 변경 시 이름/ID가 확실히 일치하지 않으면 자동 복원을 보류한다.
- 원자 저장은 유한 프로세스 lock→기존 schema/변경 병합→난수 temp→flush/필요 fsync→동일 filesystem 교체다. 고정 `.tmp` 이름을 공유하지 않는다.
- 손상 JSON은 격리 backup 후 기본값, 미래 schema는 read-only/별도 저장 안내, migration 전 backup이다. 다중 실행이 서로 기록/설정을 손상시키지 않게 시험한다.

## 5. 로그·개인정보

현재 장애 조사: 정제한 진단 이벤트를 메모리에 최대 128줄 보관한다. Linux·Windows의 우클릭 → **로그 보기**에서 현재 실행의 스냅샷을 읽을 수 있다. 로그 파일을 생성하거나 기존 파일을 읽거나 삭제하지 않는다. 기록 범위는 [README](../README.md#실행-설정-및-오류-보고)를 따른다. 아래의 디스크 기록 설계는 구현되지 않았다.

- 로그에는 오류 코드·안전한 format·단계 시간/bytes 통계·작업 ID·프로토콜/연결 도구 종류만 남긴다. 파일명/전체 경로/압축 entry·URI/서버/계정/비밀번호·EXIF/GPS·이미지·명령행 원문은 제외한다.
- Rust error chain/Debug·panic/backtrace·native stderr도 정제한다. 오류 객체를 그대로 tracing에 넣지 않는다.
- 기본 지속 상세 진단은 꺼짐. 오류 로그는 최대 4MiB/3일 회전 목표, 자동 crash dump/전송 없음. 사용자가 진단을 켜면 내용을 미리 보고 저장한다.
- OS 파일 위치 열기/경로 복사는 명시적 사용자 동작이며 임의 shell 문자열을 실행하지 않는다. 제거 프로그램은 원본/책을 삭제하지 않는다.

## 6. 라이선스·Linux 배포

- 새 코드 라이선스는 구현/배포 전 사용자가 결정한다. 현재 LICENSE가 없으며 옛 WPF/GPL 추정을 계승하지 않는다. 향후 외부 코드를 가져오면 그 권리/번역·재사용 의무는 별도로 조사한다.
- GTK/GLib·gtk-rs·선택 libadwaita·코덱/압축 wrapper와 C/C++ 본체·폰트의 고지/링크/소스 의무를 실제 아티팩트 기준으로 조사한다. libarchive 사용이 모든 RAR method/배포 조건을 자동 해결한다는 뜻은 아니다.
- Rust stable/MSRV·Cargo.lock/features/checksum·native 소스/시스템 버전·build 옵션을 고정한다. build.rs/proc-macro와 C/C++ build 도구도 공급망 검토 대상이다.
- Linux CI에서 RustSec/cargo audit·cargo deny·native CVE를 확인한다. format 확장보다 취약점 업데이트·회귀를 우선한다. HEIF 등의 특허/지역 조건은 라이선스와 분리 검토한다.
- Ubuntu 24.04 LTS x64를 기준으로 GTK4/GLib 최소 API를 고정한다. GTK/libadwaita online 최신 API를 설치된 시스템이 지원한다고 가정하지 않는다.
- 현재 개발 프리뷰는 Ubuntu 24.04 기준 Linux x86_64 AppImage다. GTK와 GUI/worker를 한 파일에 포함하지만 단일 ELF/완전 static/모든 의존성 제거를 뜻하지 않는다. glibc 2.39 이상과 시스템 그래픽·폰트 환경, 원격 읽기의 OS 연결 도구가 필요하다. desktop/MIME 통합 전체·Flatpak·서명은 미완료다.
- 깨끗한 지원 Linux 환경에서 codec/worker 누락·파일 연결·수동 업데이트·제거·설정 유지/선택 삭제를 시험한다. Windows/macOS 패키지/서명은 현재 범위가 아니다.
- 릴리스에는 format/method capability·버전/제한·체크섬·SBOM·제3자 고지·소스/빌드 방법을 포함한다. 자동 업데이트 기능은 넣지 않는다.
