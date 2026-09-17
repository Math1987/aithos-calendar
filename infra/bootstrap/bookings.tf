# Secret metadata only. The operator supplies its value outside Terraform/state.
resource "aws_secretsmanager_secret" "anakin" {
  name                    = "calendar/production/anakin"
  recovery_window_in_days = 30
  lifecycle { prevent_destroy = true }
}
resource "aws_iam_role_policy" "lambda_bookings" {
  name = "booking-operations"
  role = aws_iam_role.lambda.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    { Effect = "Allow", Action = ["secretsmanager:GetSecretValue"], Resource = aws_secretsmanager_secret.anakin.arn },
    { Effect = "Allow", Action = ["dynamodb:GetItem", "dynamodb:PutItem", "dynamodb:UpdateItem", "dynamodb:DeleteItem"], Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-bookings" }
  ] })
}
resource "aws_iam_role_policy" "deploy_bookings" {
  name = "booking-table-deployment"
  role = aws_iam_role.deploy.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [{
    Effect   = "Allow", Action = ["dynamodb:CreateTable", "dynamodb:DescribeTable", "dynamodb:UpdateTable", "dynamodb:DescribeContinuousBackups", "dynamodb:UpdateContinuousBackups", "dynamodb:DescribeTimeToLive", "dynamodb:UpdateTimeToLive", "dynamodb:ListTagsOfResource", "dynamodb:TagResource", "dynamodb:UntagResource"],
    Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-bookings"
  }] })
}
