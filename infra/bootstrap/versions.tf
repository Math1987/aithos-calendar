terraform {
  required_version = "= 1.14.6"
  required_providers {
    aws = { source = "hashicorp/aws", version = "= 6.64.0" }
  }
}

provider "aws" {
  region              = "eu-west-3"
  allowed_account_ids = ["128066560720"]
  default_tags {
    tags = { Project = "calendar", Environment = "production", ManagedBy = "terraform" }
  }
}
