# Creation/discovery of generated IDs needs broader resources; writes are otherwise
# limited by project tags, fixed names, account, region, or DNS record names.
resource "aws_iam_role_policy" "deploy" {
  name = "production-deployment"
  role = aws_iam_role.deploy.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    { Sid = "ReadCachePolicies", Effect = "Allow", Action = ["cloudfront:ListCachePolicies", "cloudfront:GetCachePolicy"], Resource = "*" },
    {
      Sid = "StateListing", Effect = "Allow", Action = ["s3:ListBucket"], Resource = aws_s3_bucket.state.arn,

    },
    {
      Sid      = "StateObject", Effect = "Allow", Action = ["s3:GetObject", "s3:PutObject"],
      Resource = "${aws_s3_bucket.state.arn}/production/terraform.tfstate"
    },
    {
      Sid      = "StateLock", Effect = "Allow", Action = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
      Resource = "${aws_s3_bucket.state.arn}/production/terraform.tfstate.tflock"
    },
    {
      Sid      = "WebsiteBucket", Effect = "Allow",
      Action   = ["s3:Get*", "s3:ListBucket*", "s3:CreateBucket", "s3:DeleteBucket", "s3:PutBucketPolicy", "s3:DeleteBucketPolicy", "s3:PutBucketPublicAccessBlock", "s3:PutEncryptionConfiguration", "s3:PutBucketTagging", "s3:PutBucketVersioning"],
      Resource = "arn:aws:s3:::${local.website_bucket}"
    },
    {
      Sid      = "WebsiteObject", Effect = "Allow", Action = ["s3:GetObject*", "s3:PutObject*", "s3:DeleteObject*"],
      Resource = "arn:aws:s3:::${local.website_bucket}/index.html"
    },
    {
      Sid      = "HealthFunction", Effect = "Allow",
      Action   = ["lambda:CreateFunction", "lambda:GetFunction*", "lambda:GetRuntimeManagementConfig", "lambda:GetPolicy", "lambda:ListVersionsByFunction", "lambda:ListTags", "lambda:TagResource", "lambda:UntagResource", "lambda:UpdateFunctionCode", "lambda:UpdateFunctionConfiguration", "lambda:DeleteFunction", "lambda:AddPermission", "lambda:RemovePermission"],
      Resource = "arn:aws:lambda:${local.region}:${local.account}:function:${local.name}-health"
    },
    {
      Sid       = "PassRuntimeRole", Effect = "Allow", Action = "iam:PassRole", Resource = aws_iam_role.lambda.arn,
      Condition = { StringEquals = { "iam:PassedToService" = "lambda.amazonaws.com" } }
    },
    {
      Sid      = "LogGroups", Effect = "Allow",
      Action   = ["logs:CreateLogGroup", "logs:DeleteLogGroup", "logs:PutRetentionPolicy", "logs:DeleteRetentionPolicy", "logs:ListTagsForResource", "logs:ListTagsLogGroup", "logs:TagResource", "logs:UntagResource", "logs:TagLogGroup", "logs:UntagLogGroup"],
      Resource = ["arn:aws:logs:${local.region}:${local.account}:log-group:/aws/lambda/${local.name}-health", "arn:aws:logs:${local.region}:${local.account}:log-group:/aws/lambda/${local.name}-health:*", "arn:aws:logs:${local.region}:${local.account}:log-group:/aws/apigateway/${local.name}", "arn:aws:logs:${local.region}:${local.account}:log-group:/aws/apigateway/${local.name}:*"]
    },
    { Sid = "DescribeLogs", Effect = "Allow", Action = "logs:DescribeLogGroups", Resource = "*" },
    {
      Sid       = "CreateApi", Effect = "Allow", Action = "apigateway:POST", Resource = "arn:aws:apigateway:${local.region}::/apis",
      Condition = { StringEquals = { "apigateway:Request/ApiName" = local.name } }
    },
    {
      Sid      = "ManageApi", Effect = "Allow", Action = ["apigateway:GET", "apigateway:POST", "apigateway:PUT", "apigateway:PATCH", "apigateway:DELETE"],
      Resource = ["arn:aws:apigateway:${local.region}::/apis/mftju6d48j", "arn:aws:apigateway:${local.region}::/apis/mftju6d48j/*"],

    },
    {
      Sid      = "ApiDomain", Effect = "Allow", Action = ["apigateway:GET", "apigateway:POST", "apigateway:PATCH", "apigateway:DELETE"],
      Resource = ["arn:aws:apigateway:${local.region}::/domainnames", "arn:aws:apigateway:${local.region}::/domainnames/api.calendar.aithos.world", "arn:aws:apigateway:${local.region}::/domainnames/api.calendar.aithos.world/*"]
    },
    {
      Sid      = "ApiTags", Effect = "Allow", Action = ["apigateway:GET", "apigateway:POST", "apigateway:PUT", "apigateway:DELETE"],
      Resource = "arn:aws:apigateway:${local.region}::/tags/*"
    },
    {
      Sid      = "ReadZone", Effect = "Allow", Action = ["route53:GetHostedZone", "route53:ListResourceRecordSets"],
      Resource = "arn:aws:route53:::hostedzone/${local.zone_id}"
    },
    {
      Sid = "CalendarDns", Effect = "Allow", Action = "route53:ChangeResourceRecordSets", Resource = "arn:aws:route53:::hostedzone/${local.zone_id}",
      Condition = { "ForAllValues:StringLike" = {
        "route53:ChangeResourceRecordSetsNormalizedRecordNames" = ["calendar.aithos.world", "api.calendar.aithos.world", "_*.calendar.aithos.world", "_*.api.calendar.aithos.world"]
      }, "ForAllValues:StringEquals" = { "route53:ChangeResourceRecordSetsRecordTypes" = ["A", "AAAA", "CNAME"] } }
    },
    { Sid = "DnsStatus", Effect = "Allow", Action = "route53:GetChange", Resource = "arn:aws:route53:::change/*" },
    {
      Sid       = "RequestCertificates", Effect = "Allow", Action = "acm:RequestCertificate", Resource = "*",
      Condition = { StringEquals = { "aws:RequestTag/Project" = "calendar", "aws:RequestedRegion" = [local.region, "us-east-1"] }, "ForAllValues:StringEquals" = { "acm:DomainNames" = ["calendar.aithos.world", "api.calendar.aithos.world"] } }
    },
    {
      Sid      = "ReadCertificates", Effect = "Allow", Action = ["acm:DescribeCertificate", "acm:ListTagsForCertificate"],
      Resource = ["arn:aws:acm:${local.region}:${local.account}:certificate/*", "arn:aws:acm:us-east-1:${local.account}:certificate/*"]
    },
    {
      Sid       = "ManageCertificates", Effect = "Allow", Action = ["acm:AddTagsToCertificate", "acm:RemoveTagsFromCertificate", "acm:DeleteCertificate"],
      Resource  = ["arn:aws:acm:${local.region}:${local.account}:certificate/*", "arn:aws:acm:us-east-1:${local.account}:certificate/*"],
      Condition = { StringEquals = { "aws:ResourceTag/Project" = "calendar" } }
    },
    {
      Sid       = "CreateDistribution", Effect = "Allow", Action = ["cloudfront:CreateDistribution", "cloudfront:CreateDistributionWithTags", "cloudfront:TagResource"],
      Resource  = "arn:aws:cloudfront::${local.account}:distribution/*",
      Condition = { StringEquals = { "aws:RequestTag/Project" = "calendar" } }
    },
    {
      Sid       = "ManageDistribution", Effect = "Allow", Action = ["cloudfront:GetDistribution", "cloudfront:GetDistributionConfig", "cloudfront:UpdateDistribution", "cloudfront:DeleteDistribution", "cloudfront:ListTagsForResource", "cloudfront:TagResource", "cloudfront:UntagResource"],
      Resource  = "arn:aws:cloudfront::${local.account}:distribution/*",
      Condition = { StringEquals = { "aws:ResourceTag/Project" = "calendar" } }
    },
    {
      Sid = "OriginAccessControl", Effect = "Allow", Action = ["cloudfront:CreateOriginAccessControl", "cloudfront:GetOriginAccessControl", "cloudfront:UpdateOriginAccessControl", "cloudfront:DeleteOriginAccessControl"], Resource = "*"
    }
  ] })
}
