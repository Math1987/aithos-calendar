#!/usr/bin/env python3
"""Upload the configured OAuth secret to existing AWS secret metadata, never TF state.
Run after bootstrap apply, via scripts/with-env.py. Never prints credential values.
"""
import os
import sys
import botocore.session

EXPECTED_ACCOUNT = '128066560720'
SECRET_ID = 'calendar/production/google-oauth-client'
CLIENT_ID = '235708078636-686f8i71em5mmsn1b29prrfv4tl8gpt3.apps.googleusercontent.com'
REDIRECT_URI = 'https://api.calendar.aithos.world/auth/google/callback'

def main():
    if os.environ.get('GOOGLE_OAUTH_CLIENT_ID') != CLIENT_ID or os.environ.get('GOOGLE_OAUTH_REDIRECT_URI') != REDIRECT_URI:
        raise ValueError('Google configuration does not match the deployment contract')
    secret = os.environ.get('GOOGLE_OAUTH_CLIENT_SECRET', '')
    if not secret:
        raise ValueError('GOOGLE_OAUTH_CLIENT_SECRET is missing')
    session = botocore.session.get_session()
    if session.create_client('sts', region_name='eu-west-3').get_caller_identity()['Account'] != EXPECTED_ACCOUNT:
        raise ValueError('Unexpected AWS account')
    client = session.create_client('secretsmanager', region_name='eu-west-3')
    client.describe_secret(SecretId=SECRET_ID)  # Metadata must be created by Terraform first.
    try:
        current = client.get_secret_value(SecretId=SECRET_ID)['SecretString']
    except client.exceptions.ResourceNotFoundException:
        current = None
    if current != secret:
        client.put_secret_value(SecretId=SECRET_ID, SecretString=secret)
    if client.get_secret_value(SecretId=SECRET_ID)['SecretString'] != secret:
        raise ValueError('Secret read-back verification failed')
    print('PASS Google OAuth secret stored and verified in AWS Secrets Manager (value hidden)')

if __name__ == '__main__':
    try:
        main()
    except Exception:
        sys.exit('Google secret setup failed; check credentials, account and bootstrap metadata. No secret values printed.')
