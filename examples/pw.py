import argparse
import json
import os
import time
from urllib.parse import urlparse
from urllib.request import Request, urlopen

from playwright.sync_api import sync_playwright


def discover_cdp_ws_url(
    cdp_proxy_url: str,
    token: str,
    retries: int = 5,
    retry_delay: float = 0.5,
) -> tuple[str, dict[str, str]]:
    headers = {
        "Authorization": f"Bearer {token}",
        "Host": "localhost",
    }
    last_error = None
    base = cdp_proxy_url.rstrip("/")
    base_host = urlparse(base).netloc
    ws_scheme = "wss" if base.startswith("https://") else "ws"

    for _ in range(retries):
        try:
            request = Request(f"{base}/json/version", headers=headers)
            with urlopen(request, timeout=10) as response:
                payload = json.load(response)

            ws_path = urlparse(payload["webSocketDebuggerUrl"])
            ws_url = f"{ws_scheme}://{base_host}{ws_path.path}"
            if ws_path.query:
                ws_url = f"{ws_url}?{ws_path.query}"
            return ws_url, headers
        except Exception as exc:
            last_error = exc
            time.sleep(retry_delay)

    raise RuntimeError(f"Failed to resolve browser websocket endpoint: {last_error}")


def get_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Connect to Modal CDP sandbox via Playwright"
    )
    parser.add_argument(
        "--url", default=os.getenv("SAURON_URL"), help="Modal sandbox connect URL"
    )
    parser.add_argument(
        "--token",
        default=os.getenv("SAURON_CONNECT_TOKEN"),
        help="Modal sandbox connect token",
    )
    parser.add_argument("--target", default="https://google.com", help="Page to load")
    parser.add_argument(
        "--screenshot", default="google.png", help="Screenshot output path"
    )
    parser.add_argument(
        "--retries", type=int, default=10, help="Retries for /json/version discovery"
    )
    parser.add_argument(
        "--retry-delay", type=float, default=0.5, help="Delay between discovery retries"
    )
    return parser.parse_args()


def main() -> None:
    args = get_args()
    if not args.url or not args.token:
        raise ValueError(
            "Missing credentials: provide --url/--token or set BROWSE_URL/BROWSE_TOKEN"
        )

    cdp_ws_url, headers = discover_cdp_ws_url(
        args.url,
        args.token,
        retries=args.retries,
        retry_delay=args.retry_delay,
    )

    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(cdp_ws_url, headers=headers)
        context = (
            browser.contexts[0]
            if browser.contexts
            else browser.new_context(
                locale="en-US",
                extra_http_headers={"Accept-Language": "en-US,en;q=0.9"},
            )
        )
        page = context.pages[0] if context.pages else context.new_page()
        page.goto(args.target)
        print(page.title())
        page.screenshot(path=args.screenshot)
        browser.close()


if __name__ == "__main__":
    main()
