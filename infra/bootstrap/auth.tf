# Secret metadata only: supply the value separately so it never enters TF state.
resource "aws_secretsmanager_secret" "google_oauth" {
  name                    = "calendar/production/google-oauth-client"
  recovery_window_in_days = 30
  lifecycle { prevent_destroy = true }
}
resource "aws_iam_role_policy" "lambda_auth" {
  name = "google-auth"
  role = aws_iam_role.lambda.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    { Effect = "Allow", Action = ["secretsmanager:GetSecretValue"], Resource = aws_secretsmanager_secret.google_oauth.arn },
    { Effect = "Allow", Action = ["dynamodb:GetItem", "dynamodb:PutItem", "dynamodb:DeleteItem"], Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-auth" }
  ] })
}
resource "aws_iam_role_policy" "deploy_auth" {
  name = "auth-table-deployment"
  role = aws_iam_role.deploy.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [{
    Effect   = "Allow",
    Action   = ["dynamodb:CreateTable", "dynamodb:DescribeTable", "dynamodb:UpdateTable", "dynamodb:DescribeContinuousBackups", "dynamodb:UpdateContinuousBackups", "dynamodb:DescribeTimeToLive", "dynamodb:UpdateTimeToLive", "dynamodb:ListTagsOfResource", "dynamodb:TagResource", "dynamodb:UntagResource"],
    Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-auth"
  }] })
}
