---@diagnostic disable-next-line: undefined-global -- Neovim owns filesystem and process effects.
local nvim = vim
local M = {}

---@class louiselm.session.QueueStore
---@field directory string
---@field path string
---@field owner? { pid: integer, boot?: string, namespace?: string }
---@field identity_waiters? fun(owner: { pid: integer, boot?: string, namespace?: string })[]
---@field save fun(self: louiselm.session.QueueStore, id: string, expected: integer, snapshot: table, callback: fun(error_message?: string))
---@field load fun(self: louiselm.session.QueueStore, agent: string, acp_id: string, cwd: string, callback: fun(records: table[]?, error_message?: string))
---@field owner_dead fun(self: louiselm.session.QueueStore, owner: table?, callback: fun(dead: boolean))

local Store = {}
Store.__index = Store

local function text(value)
  return "CAST(X'"
    .. value:gsub(".", function(byte)
      return string.format("%02x", byte:byte())
    end)
    .. "' AS TEXT)"
end

local function has_code(err, code)
  return type(err) == "string" and err:match("^" .. code .. ":") ~= nil
end

local function mkdir_private(path, callback)
  nvim.uv.fs_mkdir(path, 448, function(err)
    if err == nil or has_code(err, "EEXIST") then
      callback(true)
    elseif has_code(err, "ENOENT") then
      local parent = nvim.fs.dirname(path)
      if parent == nil or parent == path then
        callback(false)
        return
      end
      mkdir_private(parent, function(ok)
        if not ok then
          callback(false)
          return
        end
        nvim.uv.fs_mkdir(path, 448, function(retry_error)
          callback(retry_error == nil or has_code(retry_error, "EEXIST"))
        end)
      end)
    else
      callback(false)
    end
  end)
end

local function private_store(self, create, callback)
  local function finish(ok)
    nvim.schedule(function()
      callback(ok)
    end)
  end
  local function private_file(stat)
    return stat ~= nil
      and stat.type == "file"
      and stat.uid == nvim.uv.getuid()
      and stat.nlink == 1
      and stat.mode % 512 == 384
  end
  local suffixes = { "-journal", "-wal", "-shm" }
  local function sidecars(index)
    if index > #suffixes then
      finish(true)
      return
    end
    nvim.uv.fs_lstat(self.path .. suffixes[index], function(err, stat)
      if (err == nil and private_file(stat)) or has_code(err, "ENOENT") then
        sidecars(index + 1)
      else
        finish(false)
      end
    end)
  end
  local function check_file()
    nvim.uv.fs_lstat(self.path, function(err, stat)
      if err ~= nil or not private_file(stat) then
        finish(false)
        return
      end
      sidecars(1)
    end)
  end
  local function check_directory()
    nvim.uv.fs_lstat(self.directory, function(err, stat)
      if err ~= nil or stat.type ~= "directory" or stat.uid ~= nvim.uv.getuid() or stat.mode % 512 ~= 448 then
        finish(false)
        return
      end
      if not create then
        check_file()
        return
      end
      nvim.uv.fs_open(self.path, "wx", 384, function(open_error, fd)
        if fd ~= nil then
          nvim.uv.fs_close(fd, function(close_error)
            if close_error == nil then
              check_file()
            else
              finish(false)
            end
          end)
        elseif has_code(open_error, "EEXIST") then
          check_file()
        else
          finish(false)
        end
      end)
    end)
  end
  if create then
    mkdir_private(self.directory, function(ok)
      if ok then
        check_directory()
      else
        finish(false)
      end
    end)
  else
    check_directory()
  end
end

---Construct a private queue-payload store; construction performs no I/O.
---@param directory string Absolute private directory.
---@return louiselm.session.QueueStore store
function M.new(directory)
  return setmetatable({ directory = directory, path = nvim.fs.joinpath(directory, "queues.sqlite3") }, Store)
end

-- Missing Linux process-identity evidence disables recovery, never admission.
local function process_identity(self, callback)
  if self.owner ~= nil then
    nvim.schedule(function()
      callback(self.owner)
    end)
    return
  end
  if self.identity_waiters ~= nil then
    self.identity_waiters[#self.identity_waiters + 1] = callback
    return
  end
  self.identity_waiters = { callback }
  local owner = { pid = nvim.uv.os_getpid() }
  local function finish()
    nvim.schedule(function()
      self.owner = owner
      local waiters = self.identity_waiters
      self.identity_waiters = nil
      for _, waiter in ipairs(waiters) do
        waiter(owner)
      end
    end)
  end
  nvim.uv.fs_readlink("/proc/self/ns/pid", function(link_error, namespace)
    if link_error ~= nil or type(namespace) ~= "string" or namespace:match("^pid:%[%d+%]$") == nil then
      finish()
      return
    end
    nvim.uv.fs_open("/proc/sys/kernel/random/boot_id", "r", 0, function(open_error, fd)
      if open_error ~= nil or fd == nil then
        finish()
        return
      end
      nvim.uv.fs_read(fd, 128, 0, function(read_error, bytes)
        nvim.uv.fs_close(fd, function(close_error)
          if read_error == nil and close_error == nil and type(bytes) == "string" then
            local boot = bytes:match("^([%da-f%-]+)%s*$")
            if boot ~= nil and #boot == 36 then
              owner.boot, owner.namespace = boot, namespace
            end
          end
          finish()
        end)
      end)
    end)
  end)
end

---Prove the exact old editor process is absent in the same boot and PID namespace.
---PID reuse and unavailable evidence are conservatively treated as live.
---@param self louiselm.session.QueueStore
---@param old table?
---@param callback fun(dead: boolean)
function Store:owner_dead(old, callback)
  process_identity(self, function(current)
    if
      type(old) ~= "table"
      or type(old.pid) ~= "number"
      or old.pid < 2
      or old.pid % 1 ~= 0
      or current.boot == nil
      or current.namespace == nil
      or old.boot ~= current.boot
      or old.namespace ~= current.namespace
    then
      callback(false)
      return
    end
    local ok, _, code = nvim.uv.kill(old.pid, 0)
    callback(ok == nil and code == "ESRCH")
  end)
end

---Commit a complete queue snapshot under its expected revision.
---A failed acknowledgement has unknown outcome and must never trigger replay.
---@param self louiselm.session.QueueStore
---@param id string Immutable queue identity.
---@param expected integer Previous revision; zero creates a new record.
---@param snapshot table Owned immutable snapshot.
---@param callback fun(error_message?: string) Main-loop acknowledgement, once.
function Store:save(id, expected, snapshot, callback)
  process_identity(self, function(owner)
    snapshot = nvim.deepcopy(snapshot)
    snapshot.owner = owner
    local encoded, value = pcall(nvim.json.encode, snapshot)
    if not encoded then
      callback("queue payload cannot be encoded")
      return
    end
    private_store(self, true, function(private)
      if not private then
        callback("queue storage requires an owned private directory and file")
        return
      end
      local sql = {
        ".timeout 1000",
        "PRAGMA journal_mode=DELETE;",
        "PRAGMA synchronous=EXTRA;",
        "PRAGMA trusted_schema=OFF;",
        "BEGIN IMMEDIATE;",
        "CREATE TEMP TABLE queue_guard(ok INTEGER NOT NULL CHECK(ok=1));",
        "INSERT INTO queue_guard SELECT (CAST(sqlite_version() AS INTEGER)>3 OR"
          .. " (CAST(sqlite_version() AS INTEGER)=3 AND CAST(substr(sqlite_version(),3) AS INTEGER)>=38))"
          .. " AND json_valid('{}')"
          .. " AND (SELECT journal_mode='delete' FROM pragma_journal_mode)"
          .. " AND (SELECT synchronous=3 FROM pragma_synchronous)"
          .. " AND (SELECT user_version IN (0,1) FROM pragma_user_version);",
        "CREATE TABLE IF NOT EXISTS queues (id TEXT PRIMARY KEY, agent TEXT NOT NULL, acp_id TEXT NOT NULL, cwd TEXT NOT NULL, revision INTEGER NOT NULL, payload TEXT NOT NULL) STRICT;",
        "PRAGMA user_version=1;",
      }
      if expected == 0 then
        sql[#sql + 1] = "INSERT INTO queues VALUES("
          .. text(id)
          .. ","
          .. text(snapshot.agent)
          .. ","
          .. text(snapshot.acp_session_id)
          .. ","
          .. text(snapshot.working_dir)
          .. ",1,"
          .. text(value)
          .. ");"
      else
        sql[#sql + 1] = "UPDATE queues SET revision="
          .. (expected + 1)
          .. ",payload="
          .. text(value)
          .. " WHERE id="
          .. text(id)
          .. " AND revision="
          .. expected
          .. ";"
        sql[#sql + 1] = "INSERT INTO queue_guard SELECT changes();"
      end
      sql[#sql + 1] = "COMMIT;"
      local started, process = pcall(
        nvim.system,
        {
          "sqlite3",
          "-batch",
          "-bail",
          "-nofollow",
          "-init",
          "/dev/null",
          self.path,
        },
        { cwd = self.directory, env = {}, text = true, stdin = table.concat(sql, "\n"), timeout = 5000 },
        function(result)
          nvim.schedule(function()
            if result.code == 0 then
              callback()
            else
              callback("queue storage outcome is uncertain; review before retrying")
            end
          end)
        end
      )
      if not started or process == nil then
        callback("queue storage is unavailable")
      end
    end)
  end)
end

---Read saved queues for the exact Agent, ACP identity and working directory.
---The result is asynchronous and never grants execution authority by itself.
---@param self louiselm.session.QueueStore
---@param agent string
---@param acp_id string
---@param cwd string
---@param callback fun(records: table[]?, error_message?: string)
function Store:load(agent, acp_id, cwd, callback)
  nvim.uv.fs_lstat(self.path, function(stat_error)
    if has_code(stat_error, "ENOENT") then
      nvim.schedule(function()
        callback({})
      end)
      return
    end
    if stat_error ~= nil then
      nvim.schedule(function()
        callback(nil, "saved queue storage is unavailable or not private")
      end)
      return
    end
    private_store(self, false, function(private)
      if not private then
        callback(nil, "saved queue storage is unavailable or not private")
        return
      end
      local sql = [[
.timeout 1000
  PRAGMA trusted_schema=OFF;
  PRAGMA synchronous=EXTRA;
  BEGIN;
  CREATE TEMP TABLE queue_guard(ok INTEGER NOT NULL CHECK(ok=1));
  INSERT INTO queue_guard SELECT
   (CAST(sqlite_version() AS INTEGER)>3 OR
    (CAST(sqlite_version() AS INTEGER)=3 AND CAST(substr(sqlite_version(),3) AS INTEGER)>=38))
   AND json_valid('{}')
   AND (SELECT journal_mode='delete' FROM pragma_journal_mode)
   AND (SELECT synchronous=3 FROM pragma_synchronous)
   AND (SELECT user_version=1 FROM pragma_user_version);
  SELECT id,revision,payload FROM queues WHERE agent=]] .. text(agent) .. " AND acp_id=" .. text(acp_id) .. " AND cwd=" .. text(
        cwd
      ) .. "; COMMIT;"
      local started, process = pcall(nvim.system, {
        "sqlite3",
        "-readonly",
        "-batch",
        "-bail",
        "-json",
        "-nofollow",
        "-init",
        "/dev/null",
        self.path,
      }, { cwd = self.directory, env = {}, text = true, stdin = sql, timeout = 5000 }, function(result)
        nvim.schedule(function()
          if result.code ~= 0 then
            callback(nil, "saved queue storage could not be read")
            return
          end
          local ok, rows = pcall(nvim.json.decode, result.stdout ~= "" and result.stdout or "[]")
          if not ok or type(rows) ~= "table" or not nvim.islist(rows) then
            callback(nil, "saved queue storage is malformed")
            return
          end
          local records = {}
          for _, row in ipairs(rows) do
            local decoded, snapshot = pcall(nvim.json.decode, row.payload)
            if
              not decoded
              or type(snapshot) ~= "table"
              or type(row.id) ~= "string"
              or type(row.revision) ~= "number"
              or row.revision < 1
              or row.revision % 1 ~= 0
              or snapshot.queue_id ~= row.id
              or snapshot.agent ~= agent
              or snapshot.acp_session_id ~= acp_id
              or snapshot.working_dir ~= cwd
              or type(snapshot.state) ~= "table"
              or snapshot.state.revision ~= row.revision
            then
              callback(nil, "saved queue storage is malformed")
              return
            end
            if snapshot.state.disposed ~= true then
              records[#records + 1] = snapshot
            end
          end
          callback(records)
        end)
      end)
      if not started or process == nil then
        callback(nil, "saved queue storage is unavailable")
      end
    end)
  end)
end

return M
