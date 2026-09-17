# The ledger is initialized once by the bootstrap administrator. Application and
# deployment roles cannot delete it; ignore_changes prevents deploy-time resets.
resource "aws_dynamodb_table" "agent_state" {
  name                        = "${local.name}-agent-state"
  billing_mode                = "PAY_PER_REQUEST"
  hash_key                    = "id"
  deletion_protection_enabled = true
  attribute {
    name = "id"
    type = "S"
  }
  ttl {
    attribute_name = "expires_at"
    enabled        = true
  }
  server_side_encryption { enabled = true }
  point_in_time_recovery { enabled = true }
  lifecycle { prevent_destroy = true }
}
resource "aws_dynamodb_table_item" "budget_seed" {
  table_name = aws_dynamodb_table.agent_state.name
  hash_key   = "id"
  item       = jsonencode({ id = { S = "budget" }, revision = { N = "0" }, record = { S = jsonencode({ month = 0, spent = 0, held = {} }) } })
  lifecycle {
    prevent_destroy = true
    ignore_changes  = [item]
  }
}
resource "aws_iam_role" "agent_worker" {
  name               = "${local.name}-agent-worker"
  assume_role_policy = jsonencode({ Version = "2012-10-17", Statement = [{ Effect = "Allow", Principal = { Service = "lambda.amazonaws.com" }, Action = "sts:AssumeRole" }] })
}
resource "aws_iam_role_policy" "agent_worker" {
  name = "autonomous-calendar"
  role = aws_iam_role.agent_worker.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    { Effect = "Allow", Action = ["logs:CreateLogStream", "logs:PutLogEvents"], Resource = "arn:aws:logs:${local.region}:${local.account}:log-group:/aws/lambda/${local.name}-agent-worker:*" },
    { Effect = "Allow", Action = ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes", "sqs:SendMessage"], Resource = "arn:aws:sqs:${local.region}:${local.account}:${local.name}-agent" },
    { Effect = "Allow", Action = ["dynamodb:GetItem", "dynamodb:PutItem"], Resource = aws_dynamodb_table.agent_state.arn },
    { Effect = "Allow", Action = ["dynamodb:GetItem", "dynamodb:PutItem", "dynamodb:DeleteItem"], Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-auth" },
    { Effect = "Allow", Action = ["dynamodb:GetItem", "dynamodb:Scan"], Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-agents" },
    { Effect = "Allow", Action = ["dynamodb:GetItem", "dynamodb:PutItem", "dynamodb:UpdateItem", "dynamodb:DeleteItem"], Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-bookings" },
    { Effect = "Allow", Action = ["kms:Encrypt", "kms:Decrypt"], Resource = aws_kms_key.google_tokens.arn, Condition = { StringEquals = { "kms:EncryptionContext:service" = "calendar" }, Null = { "kms:EncryptionContext:account" = "false" } } },
    { Effect = "Allow", Action = ["secretsmanager:GetSecretValue"], Resource = aws_secretsmanager_secret.google_oauth.arn },
    { Effect = "Allow", Action = ["bedrock:InvokeModel"], Resource = ["arn:aws:bedrock:${local.region}:${local.account}:inference-profile/eu.anthropic.claude-haiku-4-5-20251001-v1:0", "arn:aws:bedrock:eu-*::foundation-model/anthropic.claude-haiku-4-5-20251001-v1:0"], Condition = { StringEquals = { "bedrock:InferenceProfileArn" = "arn:aws:bedrock:${local.region}:${local.account}:inference-profile/eu.anthropic.claude-haiku-4-5-20251001-v1:0" } } }
  ] })
}
resource "aws_iam_role_policy" "api_agent_tasks" {
  name = "agent-tasks"
  role = aws_iam_role.lambda.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    { Effect = "Allow", Action = ["sqs:SendMessage"], Resource = "arn:aws:sqs:${local.region}:${local.account}:${local.name}-agent" },
    { Effect = "Allow", Action = ["dynamodb:GetItem", "dynamodb:PutItem"], Resource = aws_dynamodb_table.agent_state.arn, Condition = { "ForAllValues:StringLike" = { "dynamodb:LeadingKeys" = ["job:*"] } } },
    { Effect = "Deny", Action = ["bedrock:InvokeModel", "bedrock:InvokeModelWithResponseStream", "bedrock:StartAsyncInvoke", "bedrock:CreateModelInvocationJob"], Resource = "*" }
  ] })
}
resource "aws_iam_role_policy" "deploy_agent" {
  name = "agent-worker-deployment"
  role = aws_iam_role.deploy.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    { Effect = "Allow", Action = ["lambda:CreateFunction", "lambda:GetFunction*", "lambda:GetRuntimeManagementConfig", "lambda:GetPolicy", "lambda:ListVersionsByFunction", "lambda:ListTags", "lambda:TagResource", "lambda:UntagResource", "lambda:UpdateFunctionCode", "lambda:UpdateFunctionConfiguration", "lambda:PutFunctionConcurrency", "lambda:GetFunctionConcurrency"], Resource = "arn:aws:lambda:${local.region}:${local.account}:function:${local.name}-agent-worker" },
    { Effect = "Allow", Action = ["lambda:CreateEventSourceMapping", "lambda:GetEventSourceMapping", "lambda:UpdateEventSourceMapping", "lambda:DeleteEventSourceMapping", "lambda:ListEventSourceMappings"], Resource = "*" },
    { Effect = "Allow", Action = ["iam:PassRole"], Resource = aws_iam_role.agent_worker.arn, Condition = { StringEquals = { "iam:PassedToService" = "lambda.amazonaws.com" } } },
    { Effect = "Allow", Action = ["sqs:CreateQueue", "sqs:GetQueueAttributes", "sqs:GetQueueUrl", "sqs:SetQueueAttributes", "sqs:ListQueueTags", "sqs:TagQueue", "sqs:UntagQueue"], Resource = ["arn:aws:sqs:${local.region}:${local.account}:${local.name}-agent", "arn:aws:sqs:${local.region}:${local.account}:${local.name}-agent-dlq"] },
    { Effect = "Allow", Action = ["logs:CreateLogGroup", "logs:PutRetentionPolicy", "logs:ListTagsForResource", "logs:ListTagsLogGroup", "logs:TagResource", "logs:UntagResource"], Resource = ["arn:aws:logs:${local.region}:${local.account}:log-group:/aws/lambda/${local.name}-agent-worker", "arn:aws:logs:${local.region}:${local.account}:log-group:/aws/lambda/${local.name}-agent-worker:*"] },
    { Effect = "Deny", Action = ["bedrock:InvokeModel", "bedrock:InvokeModelWithResponseStream", "bedrock:StartAsyncInvoke", "bedrock:CreateModelInvocationJob"], Resource = "*" }
  ] })
}
