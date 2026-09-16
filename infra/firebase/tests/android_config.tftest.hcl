# Shape/order captured during physical QA on 2026-09-16, louiselm-qbr.9.9.2.
# Values are dummy; both real clients advertised the normal key before QA's.
# Test consumed output directly, without computed cloud data or mock apply.
run "qa_export_selects_its_client_and_key" {
  command = plan

  module {
    source = "./modules/android-config"
  }

  variables {
    config_file_contents = base64encode(file("tests/fixtures/android-sdk-config.json"))
    package_name         = "dev.louiselm.capture.qa"
    api_key              = "dummy-qa-key"
  }

  assert {
    condition = jsondecode(output.config_json) == merge(
      jsondecode(file("tests/fixtures/android-sdk-config.json")),
      { client = [merge(jsondecode(file("tests/fixtures/android-sdk-config.json")).client[1], {
        api_key = [{ current_key = "dummy-qa-key" }]
      })] }
    )
    error_message = "QA export must retain only its client and key, preserving all other fields regardless of upstream key order."
  }
}

run "normal_export_selects_its_client_and_key" {
  command = plan

  module {
    source = "./modules/android-config"
  }

  variables {
    config_file_contents = base64encode(file("tests/fixtures/android-sdk-config.json"))
    package_name         = "dev.louiselm.capture"
    api_key              = "dummy-normal-key"
  }

  assert {
    condition = jsondecode(output.config_json) == merge(
      jsondecode(file("tests/fixtures/android-sdk-config.json")),
      { client = [merge(jsondecode(file("tests/fixtures/android-sdk-config.json")).client[0], {
        api_key = [{ current_key = "dummy-normal-key" }]
      })] }
    )
    error_message = "Normal export must retain only its client and key without changing other SDK fields."
  }
}

run "reject_missing_package" {
  command = plan

  module {
    source = "./modules/android-config"
  }

  variables {
    config_file_contents = base64encode(file("tests/fixtures/android-sdk-config.json"))
    package_name         = "dev.louiselm.absent"
    api_key              = "dummy-key"
  }

  expect_failures = [output.config_json]
}

run "reject_duplicate_package" {
  command = plan

  module {
    source = "./modules/android-config"
  }

  variables {
    config_file_contents = base64encode(jsonencode(merge(
      jsondecode(file("tests/fixtures/android-sdk-config.json")),
      { client = [for unused in range(2) : jsondecode(file("tests/fixtures/android-sdk-config.json")).client[1]] }
    )))
    package_name = "dev.louiselm.capture.qa"
    api_key      = "dummy-qa-key"
  }

  expect_failures = [output.config_json]
}
