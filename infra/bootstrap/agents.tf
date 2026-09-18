# No cross-project access; deployment cannot grant itself permissions.
resource "aws_iam_role_policy" "lambda_agents" {
  name = "agent-storage"
  role = aws_iam_role.lambda.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    {
      Effect   = "Allow", Action = ["dynamodb:GetItem", "dynamodb:Scan"],
      Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-agents",
      # signing_key is read only to sign outgoing A2A requests (src/trust/caller.rs).
      Condition = { "ForAllValues:StringEquals" = { "dynamodb:Attributes" = ["id", "record", "published", "signing_key"] }, "Null" = { "dynamodb:Attributes" = "false" } }
    },
    {
      # DeleteItem: account deletion (DELETE /account) removes the agent.
      Effect   = "Allow", Action = ["dynamodb:PutItem", "dynamodb:UpdateItem", "dynamodb:DeleteItem"],
      Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-agents"
    }
  ] })
}
resource "aws_iam_role_policy" "deploy_agents" {
  name = "agent-table-deployment"
  role = aws_iam_role.deploy.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [{
    Effect = "Allow",
    Action = [
      "dynamodb:CreateTable", "dynamodb:DescribeTable", "dynamodb:UpdateTable",
      "dynamodb:DescribeContinuousBackups", "dynamodb:UpdateContinuousBackups",
      "dynamodb:DescribeTimeToLive", "dynamodb:UpdateTimeToLive",
      "dynamodb:ListTagsOfResource", "dynamodb:TagResource", "dynamodb:UntagResource"
    ],
    Resource = "arn:aws:dynamodb:${local.region}:${local.account}:table/${local.name}-agents"
  }] })
}
