#!/usr/bin/env python3
"""Check deployed OAuth plumbing without signing in or accessing a calendar.
Creates and consumes one transient login attempt; no profile, agent or session.
"""
import json
import urllib.error
import urllib.parse
import urllib.request

API = 'https://api.calendar.aithos.world'
SITE = 'https://calendar.aithos.world'
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args):
        return None
opener = urllib.request.build_opener(NoRedirect)
def request(path, method='GET', headers=None, payload=None):
    req = urllib.request.Request(API + path, method=method, headers=headers or {}, data=None if payload is None else json.dumps(payload).encode())
    try:
        response = opener.open(req, timeout=25)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return response.status, response.headers, response.read()

status, headers, body = request('/auth/me', headers={'Origin':SITE})
assert status == 401 and json.loads(body)['error'] == 'sign_in_required'
assert headers['Cache-Control'] == 'no-store'
assert headers['Access-Control-Allow-Origin'] == SITE
assert headers['Access-Control-Allow-Credentials'] == 'true'
print('PASS private profile requires a session; credentialed CORS is restricted')
for path in ['/auth/agent', '/auth/logout']:
    status, _, _ = request(path,'POST',{'Origin':'https://unrelated.example'})
    assert status == 403
status, _, _ = request('/auth/agent','POST',{'Origin':SITE})
assert status == 401
print('PASS authenticated writes reject foreign origins and missing sessions')
status, headers, _ = request('/auth/google/start?host=oauth-smoke-test')
assert status == 303
url=urllib.parse.urlsplit(headers['Location'])
assert url.scheme == 'https' and url.netloc == 'accounts.google.com'
params=urllib.parse.parse_qs(url.query)
assert params['redirect_uri'] == [API+'/auth/google/callback']
assert set(params['scope'][0].split()) == {'openid','email','profile'}
assert params['code_challenge_method'] == ['S256']
assert len(params['code_challenge'][0]) == 43 and len(params['nonce'][0]) == 43
cookies=headers.get_all('Set-Cookie') or []
login=next(c for c in cookies if c.startswith('__Host-calendar-login='))
assert all(v in login for v in ['Secure','HttpOnly','SameSite=Lax','Path=/','Max-Age=600'])
assert 'Domain=' not in login
callback='/auth/google/callback?'+urllib.parse.urlencode({'state':params['state'][0],'error':'access_denied'})
status, headers, _=request(callback,headers={'Cookie':login.split(';')[0]})
assert status == 303 and headers['Location'] == SITE+'/account?error=login_cancelled'
assert not any(c.startswith('__Host-calendar-session=') for c in headers.get_all('Set-Cookie') or [])
status, headers, _=request(callback,headers={'Cookie':login.split(';')[0]})
assert status == 303 and headers['Location'] == SITE+'/account?error=invalid_login'
print('PASS Google redirect, PKCE, browser cookie, cancellation and one-time state')

for path, payload in [('/calendar/proposals', {'host_url':SITE+'/book/test'}),('/calendar/bookings',{'id':'gc'+'a'*40})]:
    for origin, expected in [(SITE,401),('https://unrelated.example',403)]:
        status,_,_=request(path,'POST',{'Origin':origin,'Content-Type':'application/json'},payload)
        assert status==expected,(path,status)
status,_,_=request('/calendar/disconnect','POST',{'Origin':SITE})
assert status==401
print('PASS Calendar proposals, booking and disconnect require an authenticated owner')
