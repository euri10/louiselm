local MiniTest = require("mini.test")
local PrivateFile = require("louiselm.private_file")

---@diagnostic disable-next-line: undefined-global -- `vim` is Neovim's injected runtime API.
local nvim = vim
local temp_dir
local T = MiniTest.new_set({
  hooks = {
    pre_case = function()
      temp_dir = nvim.fn.tempname()
      assert(nvim.fn.mkdir(temp_dir, "p") == 1)
    end,
    post_case = function()
      nvim.fn.delete(temp_dir, "rf")
    end,
  },
})

T["publishes private bytes and replaces the previous destination"] = function()
  local path = temp_dir .. "/record"
  assert(PrivateFile.write(nvim.uv, path, "old", "record", "publish"))
  assert(PrivateFile.write(nvim.uv, path, "new", "record", "replace"))
  MiniTest.expect.equality(nvim.fn.readfile(path), { "new" })
  MiniTest.expect.equality(nvim.fn.getfperm(path), "rw-------")
  MiniTest.expect.equality(nvim.fn.glob(path .. ".tmp-*", false, true), {})
end

for _, case in ipairs({
  { operation = "fs_mkstemp", action = "create temporary" },
  { operation = "fs_write", action = "write" },
  { operation = "fs_write", action = "write", short = true },
  { operation = "fs_fsync", action = "sync" },
  { operation = "fs_close", action = "close" },
  { operation = "fs_rename", action = "replace" },
}) do
  T["preserves destination and cleans up after " .. case.operation .. (case.short and " short write" or " failure")] = function()
    local path = temp_dir .. "/record"
    assert(PrivateFile.write(nvim.uv, path, "old", "record", "publish"))
    local uv, calls = {}, {}
    for _, operation in ipairs({ "fs_mkstemp", "fs_write", "fs_fsync", "fs_close", "fs_rename", "fs_unlink" }) do
      uv[operation] = function(...)
        calls[operation] = (calls[operation] or 0) + 1
        if operation == case.operation then
          -- Release the real descriptor even when simulating a failed close.
          if operation == "fs_close" then
            assert(nvim.uv.fs_close(...))
          end
          if case.short then
            return 1
          end
          return nil, "injected failure"
        end
        return nvim.uv[operation](...)
      end
    end
    local written, write_error = PrivateFile.write(uv, path, "new", "record", "replace")
    MiniTest.expect.equality(written, false)
    MiniTest.expect.equality(
      write_error,
      "could not " .. case.action .. " record: " .. (case.short and "short write" or "injected failure")
    )
    MiniTest.expect.equality(nvim.fn.readfile(path), { "old" })
    MiniTest.expect.equality(nvim.fn.glob(path .. ".tmp-*", false, true), {})
    MiniTest.expect.equality(calls.fs_close, case.operation ~= "fs_mkstemp" and 1 or nil)
  end
end

return T
