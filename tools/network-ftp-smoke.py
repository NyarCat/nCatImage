#!/usr/bin/env python3
"""Real loopback FTP + session KIOFuse test; needs pyftpdlib and a desktop session."""
import logging
import subprocess
import threading

from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.servers import FTPServer
from smoke_support import arguments, fixtures, image_command, work_directory


def main():
    args = arguments(__doc__)
    work = work_directory("ncat-ftp-")
    inputs = fixtures(work)
    logging.disable(logging.CRITICAL)
    authorizer = DummyAuthorizer()
    authorizer.add_anonymous(str(inputs["root"]), perm="elr")
    handler = FTPHandler
    handler.authorizer = authorizer
    server = FTPServer(("127.0.0.1", 0), handler)
    port = server.socket.getsockname()[1]
    thread = threading.Thread(target=server.serve_forever, kwargs={"timeout": .1, "handle_exit": False}, daemon=True)
    thread.start()
    try:
        for kind in ["book", "folder"]:
            directory = work / kind
            directory.mkdir()
            # No username/password: KIO's anonymous FTP behavior, not a stored account.
            escaped = inputs[kind].as_uri().rsplit("/", 1)[1]
            uri = f"ftp://127.0.0.1:{port}/{escaped}" + ("/" if kind == "folder" else "")
            result = subprocess.run(image_command(args.image, "--smoke-navigation", uri), cwd=directory,
                                    capture_output=True, text=True, timeout=35)
            assert result.returncode == 0, f"{kind} failed (exit {result.returncode})"
            assert "navigation_smoke=ok" in result.stdout
            assert not (directory / "log").exists()
            print(f"PASS real FTP {kind}: 500 navigation requests; no diagnostic files")
    finally:
        server.close_all()
        thread.join(timeout=2)
    print(f"Synthetic inputs retained in {work}")


if __name__ == "__main__":
    main()
