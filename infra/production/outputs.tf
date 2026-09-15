output "api_url" { value = "https://${local.api_domain}" }
output "health_url" { value = "https://${local.api_domain}/health" }
output "website_url" { value = "https://${local.website_domain}" }
output "function_name" { value = aws_lambda_function.health.function_name }
output "lambda_log_group" { value = aws_cloudwatch_log_group.lambda.name }
output "distribution_id" { value = aws_cloudfront_distribution.website.id }
output "website_bucket" { value = aws_s3_bucket.website.id }
