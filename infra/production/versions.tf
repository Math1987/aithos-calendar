terraform {
  required_version = "= 1.14.6"
  backend "s3" {
    key          = "production/terraform.tfstate"
    region       = "eu-west-3"
    encrypt      = true
    use_lockfile = true
  }
  required_providers {
    aws     = { source = "hashicorp/aws", version = "= 6.64.0" }
    archive = { source = "hashicorp/archive", version = "= 2.8.1" }
  }
}

provider "aws" {
  region              = "eu-west-3"
  allowed_account_ids = ["128066560720"]
  default_tags {
    tags = { Project = "calendar", Environment = "production", ManagedBy = "terraform" }
  }
}
provider "aws" {
  alias               = "virginia"
  region              = "us-east-1"
  allowed_account_ids = ["128066560720"]
  default_tags {
    tags = { Project = "calendar", Environment = "production", ManagedBy = "terraform" }
  }
}

variable "route53_zone_id" { type = string }
variable "lambda_execution_role_arn" { type = string }

locals {
  name           = "calendar-production"
  website_domain = "calendar.aithos.world"
  api_domain     = "api.calendar.aithos.world"
  website_bucket = "aithos-calendar-web-128066560720-eu-west-3"
}
