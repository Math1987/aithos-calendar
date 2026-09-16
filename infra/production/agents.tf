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

locals {
  admin_routes = {
    create  = { method = "PUT", path = "/admin/agents/{id}", invoke_path = "/admin/agents/*" }
    status  = { method = "GET", path = "/admin/agents/{id}", invoke_path = "/admin/agents/*" }
    publish = { method = "POST", path = "/admin/agents/{id}/publish", invoke_path = "/admin/agents/*/publish" }
  }
}
resource "aws_apigatewayv2_route" "admin" {
  for_each           = local.admin_routes
  api_id             = aws_apigatewayv2_api.api.id
  route_key          = "${each.value.method} ${each.value.path}"
  target             = "integrations/${aws_apigatewayv2_integration.health.id}"
  authorization_type = "AWS_IAM"
}
resource "aws_lambda_permission" "admin" {
  for_each      = local.admin_routes
  statement_id  = "AllowAgentAdmin-${each.key}"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.health.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_apigatewayv2_api.api.execution_arn}/$default/${each.value.method}${each.value.invoke_path}"
}
