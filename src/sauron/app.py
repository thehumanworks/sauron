import modal
import os

with modal.enable_output():
    app = modal.App.lookup("sauron", create_if_missing=True)

    image = (
        modal.Image.debian_slim(python_version="3.12").apt_install("chromium", "socat")
    ) or modal.Image.from_id(os.getenv("SAURON_IMAGE_ID"))

    CMD = r"""
  set -euo pipefail
  mkdir -p /tmp/chrome-data

  # Chromium listens on localhost only on recent versions (security hardening),
  # so we keep it internal (9223) and forward an externally reachable port to it.
  chromium \
    --headless=new \
    --lang=en-US \
    --no-sandbox \
    --disable-gpu \
    --user-data-dir=/tmp/chrome-data \
    --remote-debugging-port=9223 \
    --remote-allow-origins=* \
    about:blank &

  # Connect Tokens require the server to listen on port 8080
  exec socat TCP-LISTEN:8080,fork,reuseaddr TCP:127.0.0.1:9223
  """

    sb = modal.Sandbox.create(
        "bash",
        "-lc",
        CMD,
        image=image,
        app=app,
        timeout=60 * 60,
    )

    creds = sb.create_connect_token(user_metadata={"purpose": "sauron"})
    print(f"BROWSE_URL={creds.url}")
    print(f"BROWSE_TOKEN={creds.token}")
    print(
        f"Run with: BROWSE_URL={creds.url} BROWSE_TOKEN={creds.token} uv run python pw.py"
    )
