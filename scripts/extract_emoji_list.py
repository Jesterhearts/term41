#!/usr/bin/env python3
"""Fetch Unicode's full emoji list and print emoji wrapped by count."""

from __future__ import annotations

import argparse
import sys
import urllib.request
from html.parser import HTMLParser


DEFAULT_URL = "https://unicode.org/emoji/charts/full-emoji-list.html"


class EmojiChartParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.in_chars_cell = False
        self.current_text: list[str] = []
        self.emoji: list[str] = []

    def handle_starttag(
        self,
        tag: str,
        attrs: list[tuple[str, str | None]],
    ) -> None:
        if tag != "td":
            return
        classes = (dict(attrs).get("class") or "").split()
        if "chars" in classes:
            self.in_chars_cell = True
            self.current_text = []

    def handle_data(
        self,
        data: str,
    ) -> None:
        if self.in_chars_cell:
            self.current_text.append(data)

    def handle_endtag(
        self,
        tag: str,
    ) -> None:
        if tag != "td" or not self.in_chars_cell:
            return
        value = "".join(self.current_text).strip()
        if value:
            self.emoji.append(value)
        self.in_chars_cell = False
        self.current_text = []


def fetch_text(url: str) -> str:
    request = urllib.request.Request(
        url,
        headers={"User-Agent": "term41-emoji-list-script/1.0"},
    )
    with urllib.request.urlopen(request) as response:
        charset = response.headers.get_content_charset() or "utf-8"
        return response.read().decode(charset, errors="replace")


def extract_emoji(html: str) -> list[str]:
    parser = EmojiChartParser()
    parser.feed(html)
    parser.close()
    return parser.emoji


def wrapped_lines(
    emoji: list[str],
    per_line: int,
) -> list[str]:
    return [
        " ".join(emoji[index : index + per_line])
        for index in range(0, len(emoji), per_line)
    ]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Extract emoji from Unicode's full emoji list and wrap output.",
    )
    parser.add_argument("--url", default=DEFAULT_URL, help=f"source URL (default: {DEFAULT_URL})")
    parser.add_argument(
        "--per-line",
        type=int,
        default=50,
        help="number of emoji per output line (default: 50)",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.per_line <= 0:
        print("--per-line must be greater than zero", file=sys.stderr)
        return 2

    html = fetch_text(args.url)
    emoji = extract_emoji(html)
    if not emoji:
        print("no emoji found", file=sys.stderr)
        return 1

    print("\n".join(wrapped_lines(emoji, args.per_line)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
