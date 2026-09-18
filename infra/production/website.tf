data "aws_cloudfront_cache_policy" "disabled" {
  name = "Managed-CachingDisabled"
}

resource "aws_s3_bucket" "website" {
  bucket = local.website_bucket
}
resource "aws_s3_bucket_public_access_block" "website" {
  bucket                  = aws_s3_bucket.website.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}
resource "aws_s3_bucket_server_side_encryption_configuration" "website" {
  bucket = aws_s3_bucket.website.id
  rule {
    apply_server_side_encryption_by_default { sse_algorithm = "AES256" }
  }
}
resource "aws_s3_object" "index" {
  bucket        = aws_s3_bucket.website.id
  key           = "index.html"
  source        = "${path.module}/../../web/index.html"
  source_hash   = filemd5("${path.module}/../../web/index.html")
  content_type  = "text/html; charset=utf-8"
  cache_control = "no-store"
}
# Served at /logs: a live, allow-listed log feed (docs/logging.md).
resource "aws_s3_object" "logs" {
  bucket        = aws_s3_bucket.website.id
  key           = "logs"
  source        = "${path.module}/../../web/logs.html"
  source_hash   = filemd5("${path.module}/../../web/logs.html")
  content_type  = "text/html; charset=utf-8"
  cache_control = "no-store"
}
# AI Catalog discovery from the website: Link: <catalog>; rel="ai-catalog".
resource "aws_cloudfront_response_headers_policy" "website" {
  name = local.name
  custom_headers_config {
    items {
      header   = "Link"
      value    = "<https://${local.api_domain}/.well-known/ai-catalog.json>; rel=\"ai-catalog\"; type=\"application/ai-catalog+json\""
      override = true
    }
  }
}
resource "aws_cloudfront_origin_access_control" "website" {
  name                              = local.name
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}
resource "aws_cloudfront_distribution" "website" {
  enabled             = true
  is_ipv6_enabled     = true
  comment             = "Calendar production static website"
  default_root_object = "index.html"
  aliases             = [local.website_domain]
  price_class         = "PriceClass_100"
  wait_for_deployment = true
  origin {
    domain_name              = aws_s3_bucket.website.bucket_regional_domain_name
    origin_id                = "website"
    origin_access_control_id = aws_cloudfront_origin_access_control.website.id
  }
  default_cache_behavior {
    allowed_methods        = ["GET", "HEAD"]
    cached_methods         = ["GET", "HEAD"]
    target_origin_id       = "website"
    viewer_protocol_policy = "redirect-to-https"
    # Serve the latest single-file application without an invalidation step.
    cache_policy_id            = data.aws_cloudfront_cache_policy.disabled.id
    response_headers_policy_id = aws_cloudfront_response_headers_policy.website.id
  }
  # The private S3 origin returns 403 for missing keys. Serve the same application
  # for /book/{id} and the tutorial; JavaScript handles unknown paths explicitly.
  custom_error_response {
    error_code            = 403
    response_code         = 200
    response_page_path    = "/index.html"
    error_caching_min_ttl = 0
  }
  custom_error_response {
    error_code            = 404
    response_code         = 200
    response_page_path    = "/index.html"
    error_caching_min_ttl = 0
  }
  restrictions {
    geo_restriction { restriction_type = "none" }
  }
  viewer_certificate {
    acm_certificate_arn      = aws_acm_certificate_validation.website.certificate_arn
    ssl_support_method       = "sni-only"
    minimum_protocol_version = "TLSv1.2_2021"
  }
}
resource "aws_s3_bucket_policy" "website" {
  bucket = aws_s3_bucket.website.id
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    {
      Effect    = "Allow", Principal = { Service = "cloudfront.amazonaws.com" }, Action = "s3:GetObject",
      Resource  = "${aws_s3_bucket.website.arn}/*",
      Condition = { StringEquals = { "AWS:SourceArn" = aws_cloudfront_distribution.website.arn } }
    },
    {
      Effect    = "Deny", Principal = "*", Action = "s3:*",
      Resource  = [aws_s3_bucket.website.arn, "${aws_s3_bucket.website.arn}/*"],
      Condition = { Bool = { "aws:SecureTransport" = "false" } }
    }
  ] })
  depends_on = [aws_s3_bucket_public_access_block.website]
}
