terraform {
  required_version = ">= 1.12.0, < 2.0.0"

  backend "gcs" {
    bucket = "louiselm-tfstate"
    prefix = "production"
  }

  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "= 7.40.0"
    }
    google-beta = {
      source  = "hashicorp/google-beta"
      version = "= 7.40.0"
    }
  }
}

provider "google" {
  project = var.project_id
}

provider "google-beta" {
  project               = var.project_id
  billing_project       = var.project_id
  user_project_override = true
}
