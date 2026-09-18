#!/usr/bin/env python3
"""Syntax-check the shipped inline modules, without an npm dependency or build step."""
from pathlib import Path
import re
import subprocess

for page in sorted((Path(__file__).resolve().parents[1] / 'web').glob('*.html')):
    text = page.read_text()
    modules = re.findall(r'<script type="module">(.*?)</script>', text, flags=re.S)
    assert len(modules) <= 1, page
    assert '<script' not in text.replace('<script type="module">', ''), f'{page}: only inline modules are shipped'
    if not modules:
        print(f'PASS static page without scripts: {page.name}')
        continue
    subprocess.run(['node', '--input-type=module', '--check'], input=modules[0], text=True, check=True)
    print(f'PASS web JavaScript syntax: {page.name}')
