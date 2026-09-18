# Trust layer: two asymmetric KMS keys whose private halves never leave KMS.
# The operator key signs the catalog document and the host manifest; the
# guarantor key signs entry manifests and attestations (docs/trust-layer.md).
resource "aws_kms_key" "operator" {
  description              = "Calendar catalog operator signing key (ES256)"
  key_usage                = "SIGN_VERIFY"
  customer_master_key_spec = "ECC_NIST_P256"
  deletion_window_in_days  = 30
  lifecycle { prevent_destroy = true }
}
resource "aws_kms_alias" "operator" {
  name          = "alias/${local.name}-operator"
  target_key_id = aws_kms_key.operator.key_id
}
resource "aws_kms_key" "guarantor" {
  description              = "Calendar trust guarantor signing key (ES256)"
  key_usage                = "SIGN_VERIFY"
  customer_master_key_spec = "ECC_NIST_P256"
  deletion_window_in_days  = 30
  lifecycle { prevent_destroy = true }
}
resource "aws_kms_alias" "guarantor" {
  name          = "alias/${local.name}-guarantor"
  target_key_id = aws_kms_key.guarantor.key_id
}

# Both Lambda roles sign (the worker refreshes manifests when it serves the
# catalog path through shared code) and read the public halves at startup.
resource "aws_iam_role_policy" "lambda_trust" {
  for_each = { api = aws_iam_role.lambda.id, worker = aws_iam_role.agent_worker.id }
  name     = "trust-signing"
  role     = each.value
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    { Effect = "Allow", Action = ["kms:Sign", "kms:GetPublicKey", "kms:DescribeKey"], Resource = [aws_kms_key.operator.arn, aws_kms_key.guarantor.arn], Condition = { StringEquals = { "kms:SigningAlgorithm" = "ECDSA_SHA_256" } } },
    { Effect = "Allow", Action = ["kms:GetPublicKey", "kms:DescribeKey"], Resource = [aws_kms_key.operator.arn, aws_kms_key.guarantor.arn] },
    # Public feed: append-only writes, bounded reads (docs/logging.md).
    { Effect = "Allow", Action = ["dynamodb:BatchWriteItem", "dynamodb:PutItem", "dynamodb:Query"], Resource = ["arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-public-logs", "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-public-logs/index/trace-index"] }
  ] })
}
# The deploy role is at the inline-policy size limit; this one is managed.
resource "aws_iam_policy" "deploy_trust" {
  name = "${local.name}-deploy-trust"
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    {
      Effect   = "Allow", Action = ["dynamodb:CreateTable", "dynamodb:DescribeTable", "dynamodb:UpdateTable", "dynamodb:DeleteTable", "dynamodb:DescribeContinuousBackups", "dynamodb:UpdateContinuousBackups", "dynamodb:DescribeTimeToLive", "dynamodb:UpdateTimeToLive", "dynamodb:ListTagsOfResource", "dynamodb:TagResource", "dynamodb:UntagResource"],
      Resource = ["arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-public-logs", "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-public-logs/index/*"]
    },
    { Effect = "Allow", Action = ["kms:DescribeKey", "kms:GetPublicKey", "kms:ListAliases"], Resource = [aws_kms_key.operator.arn, aws_kms_key.guarantor.arn] },
    # The website now ships two pages and a response-headers policy (Link: rel="ai-catalog").
    { Effect = "Allow", Action = ["s3:GetObject*", "s3:PutObject*", "s3:DeleteObject*"], Resource = "arn:aws:s3:::${local.website_bucket}/logs" },
    { Effect = "Allow", Action = ["cloudfront:CreateResponseHeadersPolicy", "cloudfront:GetResponseHeadersPolicy", "cloudfront:UpdateResponseHeadersPolicy", "cloudfront:DeleteResponseHeadersPolicy", "cloudfront:ListResponseHeadersPolicies"], Resource = "*" }
  ] })
}
resource "aws_iam_role_policy_attachment" "deploy_trust" {
  role       = aws_iam_role.deploy.name
  policy_arn = aws_iam_policy.deploy_trust.arn
}
