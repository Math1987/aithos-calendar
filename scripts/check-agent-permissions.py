#!/usr/bin/env python3
"""Read-only IAM simulation for the inference and budget boundaries."""
import botocore.session
s=botocore.session.get_session()
iam=s.create_client('iam')
account='128066560720'
profile=f'arn:aws:bedrock:eu-west-3:{account}:inference-profile/eu.anthropic.claude-haiku-4-5-20251001-v1:0'
table=f'arn:aws:dynamodb:eu-west-3:{account}:table/calendar-production-agent-state'
checks=[
 ('health','bedrock:InvokeModel',profile,[],False),
 ('deploy','bedrock:InvokeModel',profile,[],False),
 ('agent-worker','bedrock:InvokeModel',profile,[],True),
 ('agent-worker','bedrock:InvokeModel','arn:aws:bedrock:eu-west-3::foundation-model/anthropic.claude-haiku-4-5-20251001-v1:0',[],False),
 ('agent-worker','bedrock:InvokeModel','arn:aws:bedrock:eu-west-3::foundation-model/anthropic.claude-haiku-4-5-20251001-v1:0',[{'ContextKeyName':'bedrock:InferenceProfileArn','ContextKeyValues':[profile],'ContextKeyType':'string'}],True),
 ('health','dynamodb:PutItem',table,[{'ContextKeyName':'dynamodb:LeadingKeys','ContextKeyValues':['budget'],'ContextKeyType':'stringList'}],False),
 ('health','dynamodb:PutItem',table,[{'ContextKeyName':'dynamodb:LeadingKeys','ContextKeyValues':['job:test'],'ContextKeyType':'stringList'}],True),
 ('agent-worker','dynamodb:DeleteItem',table,[],False),
 ('deploy','dynamodb:PutItem',table,[],False),
]
for role,action,resource,context,allowed in checks:
    r=iam.simulate_principal_policy(PolicySourceArn=f'arn:aws:iam::{account}:role/calendar-production-{role}',ActionNames=[action],ResourceArns=[resource],ContextEntries=context)
    decision=r['EvaluationResults'][0]['EvalDecision']
    assert (decision=='allowed')==allowed,(role,action,decision)
    print('PASS',role,action,decision)
