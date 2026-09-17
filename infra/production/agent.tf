resource "aws_sqs_queue" "agent_dlq" {
  name                      = "${local.name}-agent-dlq"
  message_retention_seconds = 1209600
  sqs_managed_sse_enabled   = true
}
resource "aws_sqs_queue" "agent" {
  name                       = "${local.name}-agent"
  visibility_timeout_seconds = 1440
  message_retention_seconds  = 86400
  sqs_managed_sse_enabled    = true
  redrive_policy             = jsonencode({ deadLetterTargetArn = aws_sqs_queue.agent_dlq.arn, maxReceiveCount = 4 })
}
resource "aws_cloudwatch_log_group" "agent_worker" {
  name              = "/aws/lambda/${local.name}-agent-worker"
  retention_in_days = 14
}
resource "aws_lambda_function" "agent_worker" {
  function_name                  = "${local.name}-agent-worker"
  role                           = "arn:aws:iam::128066560720:role/${local.name}-agent-worker"
  handler                        = "bootstrap"
  runtime                        = "provided.al2023"
  architectures                  = ["x86_64"]
  memory_size                    = 512
  timeout                        = 240
  reserved_concurrent_executions = 4
  filename                       = data.archive_file.health.output_path
  source_code_hash               = data.archive_file.health.output_base64sha256
  depends_on                     = [aws_cloudwatch_log_group.agent_worker]
  environment {
    variables = merge(aws_lambda_function.health.environment[0].variables, { CALENDAR_WORKER = "true" })
  }
}
resource "aws_lambda_event_source_mapping" "agent" {
  event_source_arn        = aws_sqs_queue.agent.arn
  function_name           = aws_lambda_function.agent_worker.arn
  batch_size              = 1
  function_response_types = ["ReportBatchItemFailures"]
  scaling_config { maximum_concurrency = 2 }
}
