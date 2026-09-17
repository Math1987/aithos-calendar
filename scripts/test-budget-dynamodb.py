#!/usr/bin/env python3
"""Exercise the production CAS implementation in an isolated temporary AWS table.
No Bedrock calls. Run through with-env.py. Always removes its own test table.
"""
import os
from pathlib import Path
import subprocess
import uuid
import botocore.session

root = Path(__file__).resolve().parents[1]
client = botocore.session.get_session().create_client('dynamodb', region_name='eu-west-3')
name = 'calendar-budget-test-' + uuid.uuid4().hex
created = False
try:
    client.create_table(TableName=name, BillingMode='PAY_PER_REQUEST',
        AttributeDefinitions=[{'AttributeName':'id','AttributeType':'S'}],
        KeySchema=[{'AttributeName':'id','KeyType':'HASH'}],
        Tags=[{'Key':'Project','Value':'calendar'},{'Key':'Purpose','Value':'temporary-budget-test'}])
    created = True
    client.get_waiter('table_exists').wait(TableName=name)
    env = dict(os.environ, BUDGET_TEST_TABLE=name, AWS_REGION='eu-west-3')
    subprocess.run(['cargo','run','--offline','--locked','--example','budget_probe'],cwd=root,env=env,check=True)
finally:
    if created:
        client.delete_table(TableName=name)
        print('Temporary budget test table deleted.')
