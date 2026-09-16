#!/usr/bin/env python3
"""Administer mock identities through the IAM-protected Calendar API (AWS SigV4)."""
import argparse
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path

import botocore.auth
import botocore.awsrequest
import botocore.session

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action',choices=['create','status','publish'])
    parser.add_argument('--id',required=True,help='Stable Calendar UUID; reuse it when retrying')
    parser.add_argument('--file',type=Path,help='JSON name and mock_availability for create')
    parser.add_argument('--api',default='https://api.calendar.aithos.world')
    parser.add_argument('--region',default='eu-west-3')
    args=parser.parse_args()
    import uuid
    if str(uuid.UUID(args.id)) != args.id:
        parser.error('--id must be a canonical UUID')
    if not args.api.startswith('https://'):
        parser.error('--api must use HTTPS')
    if args.action=='create' and args.file is None:
        parser.error('create requires --file')
    method={'create':'PUT','status':'GET','publish':'POST'}[args.action]
    suffix='/publish' if args.action=='publish' else ''
    url=f'{args.api.rstrip("/")}/admin/agents/{args.id}{suffix}'
    payload=json.dumps(json.loads(args.file.read_text())).encode() if args.action=='create' else None
    credentials=botocore.session.get_session().get_credentials()
    if credentials is None:
        raise SystemExit('No AWS credentials found. Use scripts/with-env.py or an AWS profile.')
    request=botocore.awsrequest.AWSRequest(method=method,url=url,data=payload,headers={'Content-Type':'application/json'})
    botocore.auth.SigV4Auth(credentials.get_frozen_credentials(),'execute-api',args.region).add_auth(request)
    http=urllib.request.Request(url,data=payload,headers=dict(request.headers),method=method)
    try:
        with urllib.request.build_opener(NoRedirect).open(http,timeout=25) as response:
            body=json.load(response)
    except urllib.error.HTTPError as error:
        # Never echo an AWS signature diagnostic, which may include signed headers.
        import re
        try:
            code=json.load(error).get('error','')
        except (ValueError, AttributeError):
            code=''
        if isinstance(code,str) and re.fullmatch(r'[a-z_]{1,80}',code):
            print(code,file=sys.stderr)
        raise SystemExit(f'Calendar returned HTTP {error.code}')
    print(json.dumps(body,indent=2))
    if body.get('publication_status')=='pending':
        print('Saved locally; publication pending. Retry publish with the same --id.',file=sys.stderr)

if __name__=='__main__':
    main()
