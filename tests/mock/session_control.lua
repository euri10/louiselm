-- Offline, fixed advertised choices for the real configured-owner batch gate.
require("louiselm.dev.mock_agent").run({
  config_options = {
    {
      id = "model",
      name = "Model",
      type = "select",
      category = "model",
      currentValue = "small",
      options = { { value = "small", name = "Small" }, { value = "sol", name = "Sol" } },
    },
    {
      id = "effort",
      name = "Effort",
      type = "select",
      currentValue = "low",
      options = { { value = "low", name = "Low" }, { value = "max", name = "Max" } },
    },
    {
      id = "mode",
      name = "Access",
      type = "select",
      currentValue = "default",
      options = { { value = "default", name = "Default" }, { value = "full", name = "Full access" } },
    },
  },
})
