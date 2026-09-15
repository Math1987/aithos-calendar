locals {
  account        = "128066560720"
  region         = "eu-west-3"
  name           = "calendar-production"
  state_bucket   = "aithos-calendar-tfstate-${local.account}-${local.region}"
  website_bucket = "aithos-calendar-web-${local.account}-${local.region}"
  zone_id        = "Z09988302Y6VWTN77SVQ8"
}

data "aws_iam_openid_connect_provider" "github" {
  arn = "arn:aws:iam::${local.account}:oidc-provider/token.actions.githubusercontent.com"
}

resource "aws_s3_bucket" "state" {
  bucket = local.state_bucket
  lifecycle { prevent_destroy = true }
}
resource "aws_s3_bucket_versioning" "state" {
  bucket = aws_s3_bucket.state.id
  versioning_configuration { status = "Enabled" }
}
resource "aws_s3_bucket_server_side_encryption_configuration" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    apply_server_side_encryption_by_default { sse_algorithm = "AES256" }
  }
}
resource "aws_s3_bucket_public_access_block" "state" {
  bucket                  = aws_s3_bucket.state.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}
resource "aws_s3_bucket_policy" "state" {
  bucket = aws_s3_bucket.state.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [{
    Effect    = "Deny", Principal = "*", Action = "s3:*",
    Resource  = [aws_s3_bucket.state.arn, "${aws_s3_bucket.state.arn}/*"],
    Condition = { Bool = { "aws:SecureTransport" = "false" } }
  }] })
}

resource "aws_iam_role" "lambda" {
  name = "${local.name}-health"
  assume_role_policy = jsonencode({ Version = "2012-10-17", Statement = [{
    Effect = "Allow", Principal = { Service = "lambda.amazonaws.com" }, Action = "sts:AssumeRole"
  }] })
}
resource "aws_iam_role_policy" "lambda_logs" {
  name = "health-logs"
  role = aws_iam_role.lambda.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [{
    Effect   = "Allow", Action = ["logs:CreateLogStream", "logs:PutLogEvents"],
    Resource = "arn:aws:logs:${local.region}:${local.account}:log-group:/aws/lambda/${local.name}-health:*"
  }] })
}

resource "aws_iam_role" "deploy" {
  name                 = "${local.name}-deploy"
  max_session_duration = 3600
  assume_role_policy = jsonencode({ Version = "2012-10-17", Statement = [{
    Effect    = "Allow", Action = "sts:AssumeRoleWithWebIdentity",
    Principal = { Federated = data.aws_iam_openid_connect_provider.github.arn },
    Condition = { StringEquals = {
      "token.actions.githubusercontent.com:aud" = "sts.amazonaws.com",
      "token.actions.githubusercontent.com:sub" = "repo:Math1987@55652304/aithos-calendar@1371058059:ref:refs/heads/main"
    } }
  }] })
}

output "state_bucket" { value = aws_s3_bucket.state.id }
output "deployment_role_arn" { value = aws_iam_role.deploy.arn }
output "lambda_execution_role_arn" { value = aws_iam_role.lambda.arn }
