#!/usr/bin/env python3
"""Read the application's conservative allowance; this is not the AWS invoice."""
from datetime import datetime, timezone
from decimal import Decimal
import json
import botocore.session

client=botocore.session.get_session().create_client('dynamodb',region_name='eu-west-3')
item=client.get_item(TableName='calendar-production-agent-state',Key={'id':{'S':'budget'}},ConsistentRead=True).get('Item')
if not item:
    raise SystemExit('Inference blocked: budget ledger missing.')
ledger=json.loads(item['record']['S'])
now=datetime.now(timezone.utc)
month=now.year*12+now.month-1
held=sum(ledger['held'].values())
spent=ledger['spent'] if ledger['month']==month else 0
if ledger['month']>month:
    raise SystemExit('Inference blocked: ledger month is ahead of clock.')
def dollars(n): return str(Decimal(n)/Decimal(1_000_000_000))
print(json.dumps({'month_utc':now.strftime('%Y-%m'),'hard_limit_usd':'30','operating_limit_usd':'25',
    'completed_maximum_usd':dollars(spent),'unresolved_maximum_usd':dollars(held),
    'remaining_allowance_usd':dollars(max(0,25_000_000_000-spent-held)),
    'unresolved_invocations':len(ledger['held']),
    'note':'Conservative admission accounting, not actual billed cost.'},indent=2))
