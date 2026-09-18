#!/usr/bin/env python3
"""Syntax-check the shipped inline modules, without an npm dependency or build step."""
from pathlib import Path
import re
import subprocess

for page in sorted((Path(__file__).resolve().parents[1] / 'web').glob('*.html')):
    modules = re.findall(r'<script type="module">(.*?)</script>', page.read_text(), flags=re.S)
    assert len(modules) == 1, page
    subprocess.run(['node', '--input-type=module', '--check'], input=modules[0], text=True, check=True)
    print(f'PASS web JavaScript syntax: {page.name}')
