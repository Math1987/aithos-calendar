resource "aws_dynamodb_table" "agents" {
  name                        = "${local.name}-agents"
  billing_mode                = "PAY_PER_REQUEST"
  hash_key                    = "id"
  deletion_protection_enabled = true
  attribute {
    name = "id"
    type = "S"
  }
  server_side_encryption { enabled = true }
  point_in_time_recovery { enabled = true }
  lifecycle { prevent_destroy = true }
}

# Public find-or-create; AWS credentials are only used by Lambda internally.
resource "aws_apigatewayv2_route" "onboarding" {
  api_id             = aws_apigatewayv2_api.api.id
  route_key          = "POST /agents"
  target             = "integrations/${aws_apigatewayv2_integration.health.id}"
  authorization_type = "NONE"
}
resource "aws_lambda_permission" "onboarding" {
  statement_id  = "AllowPublicOnboarding"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.health.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.api.execution_arn}/$default/POST/agents"
}
