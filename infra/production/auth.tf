# Private account mappings, expiring OAuth attempts and browser sessions.
resource "aws_dynamodb_table" "auth" {
  name                        = "${local.name}-auth"
  billing_mode                = "PAY_PER_REQUEST"
  hash_key                    = "id"
  deletion_protection_enabled = true
  attribute {
    name = "id"
    type = "S"
  }
  ttl {
    attribute_name = "expires_at"
    enabled        = true
  }
  server_side_encryption { enabled = true }
  point_in_time_recovery { enabled = true }
  lifecycle { prevent_destroy = true }
}
