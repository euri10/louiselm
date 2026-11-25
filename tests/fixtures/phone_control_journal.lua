---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim

return function(binary, directory)
  local env = {
    LOUISELM_CAPTURE_CONFIG_DIR = directory,
    LOUISELM_CAPTURE_DATA_DIR = directory,
    LOUISELM_CAPTURE_STATE_DIR = directory,
  }
  for _, args in ipairs({
    { "configure-network", "--profile", "lan", "--bind", "192.168.1.20:7391", "--url", "https://192.168.1.20:7391" },
    { "pair", "--svg", directory .. "/pairing.svg" },
  }) do
    local result = nvim
      .system(nvim.list_extend({ binary }, args), { clear_env = true, env = env, text = true, timeout = 10000 })
      :wait()
    assert(result.code == 0, result.stderr)
  end
  local tls = directory .. "/louiselm/capture/tls"
  assert(nvim.uv.fs_stat(tls .. "/receiver-key.pem"))
  assert(nvim.uv.fs_stat(tls .. "/receiver-cert.pem"))
  assert(nvim.uv.fs_stat(tls .. "/phone-action-owner.json.lock") == nil)
  assert(nvim.uv.fs_stat(directory .. "/louiselm/workflow/phone-action-owner") == nil)
end
