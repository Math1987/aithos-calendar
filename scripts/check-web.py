#!/usr/bin/env python3
"""Syntax-check the shipped inline module, without an npm dependency or build step."""
from pathlib import Path
import re
import subprocess

html = (Path(__file__).resolve().parents[1] / 'web/index.html').read_text()
modules = re.findall(r'<script type="module">(.*?)</script>', html, flags=re.S)
assert len(modules) == 1
subprocess.run(['node', '--input-type=module', '--check'], input=modules[0], text=True, check=True)
print('PASS web JavaScript syntax')
