#!/usr/bin/env python3
"""Build static GitHub Pages redirects for Couch's former project-site URLs."""
import argparse
import json
from pathlib import Path

PAGES = ('', 'index.html', 'credits.html', 'integrations.html', 'preview.html',
         'usage/', 'usage/index.html', 'usage/rooms.html', 'usage/lights.html',
         'usage/home-assistant.html', 'usage/kodi.html', 'usage/android-tv.html',
         'usage/webos.html', 'usage/apple-tv.html', 'usage/infrared.html')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--template', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    template = args.template.read_text(encoding='utf-8')
    args.output.mkdir(parents=True, exist_ok=True)
    for path in PAGES:
        target = 'https://couch-os.dev/' + path
        destination = args.output / (path or 'index.html')
        if path.endswith('/'):
            destination /= 'index.html'
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(template.replace('{{TARGET}}', target).replace('{{TARGET_JSON}}', json.dumps(target)), encoding='utf-8')


if __name__ == '__main__':
    main()
