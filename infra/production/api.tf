data "archive_file" "health" {
  type             = "zip"
  source_file      = "${path.module}/../../.build/bootstrap"
  output_path      = "${path.module}/health.zip"
  output_file_mode = "0755"
}

resource "aws_cloudwatch_log_group" "lambda" {
  name              = "/aws/lambda/${local.name}-health"
  retention_in_days = 14
}

resource "aws_lambda_function" "health" {
  function_name    = "${local.name}-health"
  role             = var.lambda_execution_role_arn
  handler          = "bootstrap"
  runtime          = "provided.al2023"
  architectures    = ["x86_64"]
  memory_size      = 128
  timeout          = 15
  filename         = data.archive_file.health.output_path
  source_code_hash = data.archive_file.health.output_base64sha256
  depends_on       = [aws_cloudwatch_log_group.lambda]
  environment {
    variables = {
      CALENDAR_PUBLIC_URL  = "https://${local.api_domain}"
      CATALOG_URL          = "https://${local.api_domain}/.well-known/ai-catalog.json"
      AGENTS_TABLE         = aws_dynamodb_table.agents.name
      REGISTRY_ORIGIN      = "https://registry.aithos.world"
      CALENDAR_WEBSITE_URL = "https://${local.website_domain}"
    }
  }
}

resource "aws_apigatewayv2_api" "api" {
  name                         = local.name
  protocol_type                = "HTTP"
  disable_execute_api_endpoint = true
  cors_configuration {
    allow_origins = ["https://${local.website_domain}"]
    allow_methods = ["GET", "POST", "OPTIONS"]
    allow_headers = ["content-type", "a2a-version"]
    max_age       = 300
  }
}
resource "aws_apigatewayv2_integration" "health" {
  api_id                 = aws_apigatewayv2_api.api.id
  integration_type       = "AWS_PROXY"
  integration_uri        = aws_lambda_function.health.invoke_arn
  payload_format_version = "2.0"
  timeout_milliseconds   = 20000
}
resource "aws_apigatewayv2_route" "health" {
  api_id    = aws_apigatewayv2_api.api.id
  route_key = "GET /health"
  target    = "integrations/${aws_apigatewayv2_integration.health.id}"
}
resource "aws_apigatewayv2_stage" "production" {
  api_id      = aws_apigatewayv2_api.api.id
  name        = "$default"
  auto_deploy = true
  default_route_settings {
    throttling_burst_limit = 20
    throttling_rate_limit  = 10
  }

}
resource "aws_lambda_permission" "api" {
  statement_id  = "AllowHealthApi"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.health.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.api.execution_arn}/$default/GET/health"
}
resource "aws_apigatewayv2_domain_name" "api" {
  domain_name = local.api_domain
  domain_name_configuration {
    certificate_arn = aws_acm_certificate_validation.api.certificate_arn
    endpoint_type   = "REGIONAL"
    security_policy = "TLS_1_2"
  }
}
resource "aws_apigatewayv2_api_mapping" "api" {
  api_id      = aws_apigatewayv2_api.api.id
  domain_name = aws_apigatewayv2_domain_name.api.id
  stage       = aws_apigatewayv2_stage.production.id
}

# Discovery and mock A2A share the existing function and integration.
locals {
  agent_routes = {
    catalog = { method = "GET", path = "/.well-known/ai-catalog.json", invoke_path = "/.well-known/ai-catalog.json" }
    cards   = { method = "GET", path = "/agents/{tenant}/agent-card.json", invoke_path = "/agents/*/agent-card.json" }
    a2a     = { method = "POST", path = "/a2a", invoke_path = "/a2a" }
  }
}

resource "aws_apigatewayv2_route" "agents" {
  for_each  = local.agent_routes
  api_id    = aws_apigatewayv2_api.api.id
  route_key = "${each.value.method} ${each.value.path}"
  target    = "integrations/${aws_apigatewayv2_integration.health.id}"
}

resource "aws_lambda_permission" "agents" {
  for_each      = local.agent_routes
  statement_id  = "AllowAgentRoute-${each.key}"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.health.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.api.execution_arn}/$default/${each.value.method}${each.value.invoke_path}"
}
