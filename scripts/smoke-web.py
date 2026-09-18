#!/usr/bin/env python3
"""Read-only production checks for direct web routes and browser API access."""
import urllib.request

site = 'https://calendar.aithos.world'
api = 'https://api.calendar.aithos.world'
for path in ['/', '/book/unknown-agent', '/help/google-booking-page', '/account']:
    with urllib.request.urlopen(site + path, timeout=20) as response:
        html = response.read().decode()
        assert response.status == 200 and '<script type="module">' in html and 'Find a time<br>together.' in html
    print('PASS direct web route', path)
# Google OAuth app requirements: the home page links to real legal pages,
# served as static HTML (no script needed) with the Limited Use statement.
with urllib.request.urlopen(site + '/', timeout=20) as response:
    home = response.read().decode()
    assert 'href="/privacy"' in home and 'href="/terms"' in home and 'A2A Calendar POC' in home
for path, marker in [('/privacy', 'Limited Use'), ('/terms', 'Terms of use')]:
    with urllib.request.urlopen(site + path, timeout=20) as response:
        html = response.read().decode()
        assert response.status == 200 and marker in html and '<script' not in html, path
        assert 'text/html' in response.headers.get('Content-Type', '')
    print('PASS legal page', path)
for path, headers in [('/calendar/tasks', 'content-type'), ('/bookings', 'content-type'), ('/agents', 'content-type'), ('/a2a', 'content-type,a2a-version')]:
    request = urllib.request.Request(api + path, method='OPTIONS', headers={
        'Origin': site, 'Access-Control-Request-Method': 'POST', 'Access-Control-Request-Headers': headers,
    })
    with urllib.request.urlopen(request, timeout=15) as response:
        assert response.headers['Access-Control-Allow-Origin'] == site
        allowed = response.headers['Access-Control-Allow-Headers'].lower().replace(' ', '').split(',')
        assert all(header in allowed for header in headers.split(','))
        assert 'POST' in response.headers['Access-Control-Allow-Methods']
        assert response.headers.get('Access-Control-Allow-Credentials') == 'true'
    print('PASS browser preflight', path)
request = urllib.request.Request(api + '/account', method='OPTIONS', headers={
    'Origin': site, 'Access-Control-Request-Method': 'DELETE',
})
with urllib.request.urlopen(request, timeout=15) as response:
    assert 'DELETE' in response.headers['Access-Control-Allow-Methods']
print('PASS browser preflight DELETE /account')
request = urllib.request.Request(api + '/health', headers={'Origin':site})
with urllib.request.urlopen(request, timeout=15) as response:
    assert response.headers['Access-Control-Allow-Origin'] == site
request = urllib.request.Request(api + '/health', headers={'Origin':'https://unrelated.example'})
with urllib.request.urlopen(request, timeout=15) as response:
    assert response.headers.get('Access-Control-Allow-Origin') not in ['*', 'https://unrelated.example']
print('PASS CORS restricted to Calendar website')
