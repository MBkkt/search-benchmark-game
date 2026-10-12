#!/usr/bin/env python3
"""Put a fresh build of web/ behind every page the site publishes.

Usage:
    stage_site.py --build <fresh react-scripts build dir> [--site-root web/build]

The site is one bundle shared by several pages: the root, /full, /tpuf, each
fork's /<fork>/full and each scale page's /<fork>/<corpus>/<scale>/full. Every
page is the same index.html, differing only in how deep it sits (so how many
`../` lead back to `static/`) and in the label some carry in their title and
heading. That makes every page derivable from the build's own index.html, and
this script derives them all, so no page can keep pointing at a bundle older
than the source.

A page's label is read from its current title, which is where
publish_branch_full.py wrote it.
"""
import argparse
import re
import shutil
import sys
from pathlib import Path

from publish_branch_full import relocate, retitle

# What publish_branch_full.retitle writes into a labelled page's title.
LABELLED_TITLE = re.compile(r"<title>Search benchmark — (.*?)</title>")

# Build outputs that live beside the root page rather than under static/.
ROOT_ASSET_GLOBS = ("asset-manifest.json", "service-worker.js", "precache-manifest.*.js")


def page_label(html: str):
    """The label a page was published under, or None for an unlabelled page."""
    m = LABELLED_TITLE.search(html)
    return m.group(1) if m else None


def replace_assets(build: Path, site_root: Path) -> None:
    """Swap the site's bundle for the fresh one, leaving no stale hash behind."""
    shutil.rmtree(site_root / "static", ignore_errors=True)
    shutil.copytree(build / "static", site_root / "static")
    for pattern in ROOT_ASSET_GLOBS:
        for old in site_root.glob(pattern):
            old.unlink()
        for new in build.glob(pattern):
            shutil.copy2(new, site_root / new.name)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--build", required=True, help="output of `npm run build` in a copy of web/")
    p.add_argument("--site-root", default="web/build")
    args = p.parse_args()

    build = Path(args.build)
    site_root = Path(args.site_root)
    template = (build / "index.html").read_text()

    pages = sorted(site_root.rglob("index.html"))
    if not pages:
        sys.exit(f"no pages under {site_root}")

    replace_assets(build, site_root)
    for page in pages:
        depth = len(page.relative_to(site_root).parts) - 1
        label = page_label(page.read_text())
        html = relocate(template, depth)
        if label is not None:
            html = retitle(html, label)
        page.write_text(html)
        print(f"staged {page} (depth {depth}{', ' + label if label else ''})")


if __name__ == "__main__":
    main()
