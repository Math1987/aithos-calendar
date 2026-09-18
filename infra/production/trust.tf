# Trust layer routes and the public log feed. Keys live in the bootstrap stack.
locals {
  trust_routes = {
    operator_jwks  = { method = "GET", path = "/.well-known/jwks.json", invoke_path = "/.well-known/jwks.json" }
    guarantor_jwks = { method = "GET", path = "/trust-provider/.well-known/jwks.json", invoke_path = "/trust-provider/.well-known/jwks.json" }
    agent_jwks     = { method = "GET", path = "/agents/{tenant}/jwks.json", invoke_path = "/agents/*/jwks.json" }
    logs_events    = { method = "GET", path = "/logs/events", invoke_path = "/logs/events" }
    lab_index      = { method = "GET", path = "/lab", invoke_path = "/lab" }
    lab_report     = { method = "GET", path = "/lab/report", invoke_path = "/lab/report" }
    lab_rogue_jwks = { method = "GET", path = "/lab/rogue/trust-provider/.well-known/jwks.json", invoke_path = "/lab/rogue/trust-provider/.well-known/jwks.json" }
    lab_catalog    = { method = "GET", path = "/lab/{scenario}/.well-known/ai-catalog.json", invoke_path = "/lab/*/.well-known/ai-catalog.json" }
    lab_card       = { method = "GET", path = "/lab/{scenario}/agents/{tenant}/agent-card.json", invoke_path = "/lab/*/agents/*/agent-card.json" }
  }
}
resource "aws_apigatewayv2_route" "trust" {
  for_each  = local.trust_routes
  api_id    = aws_apigatewayv2_api.api.id
  route_key = "${each.value.method} ${each.value.path}"
  target    = "integrations/${aws_apigatewayv2_integration.health.id}"
}
resource "aws_lambda_permission" "trust" {
  for_each      = local.trust_routes
  statement_id  = "AllowTrustRoute-${each.key}"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.health.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.api.execution_arn}/$default/${each.value.method}${each.value.invoke_path}"
}

# Public feed: a live tail with a short TTL, never the CloudWatch logs.
resource "aws_dynamodb_table" "public_logs" {
  name         = "${local.name}-public-logs"
  billing_mode = "PAY_PER_REQUEST"
  hash_key     = "bucket"
  range_key    = "sk"
  attribute {
    name = "bucket"
    type = "S"
  }
  attribute {
    name = "sk"
    type = "S"
  }
  attribute {
    name = "trace_id"
    type = "S"
  }
  global_secondary_index {
    name            = "trace-index"
    hash_key        = "trace_id"
    range_key       = "sk"
    projection_type = "ALL"
  }
  ttl {
    attribute_name = "expires"
    enabled        = true
  }
  server_side_encryption { enabled = true }
}

# Lab keys derive from a seed so every Lambda instance publishes the same
# successor key and signs impostor manifests with the same impostor key.
resource "random_password" "lab_seed" {
  length  = 48
  special = false
}
