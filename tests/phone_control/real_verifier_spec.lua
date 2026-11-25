local MiniTest = require("mini.test")
---@diagnostic disable-next-line: undefined-global -- Neovim test runtime.
local nvim = vim
local Controller = require("louiselm.phone_control.controller")
local T = MiniTest.new_set()

T["the real capture verifier authenticates both signatures before projection"] = function()
  local binary = nvim.env.LOUISELM_TEST_CAPTURE
  if binary == nil or binary == "" then
    MiniTest.skip("set LOUISELM_TEST_CAPTURE to the built capture executable")
  end
  binary = nvim.fn.fnamemodify(binary, ":p")
  local directory = nvim.fn.tempname()
  local tls = directory .. "/louiselm/capture/tls"
  assert(nvim.fn.mkdir(tls, "p") == 1)
  local key = nvim.fn.readfile("tests/fixtures/phone_control_receiver_key.pem")
  assert(nvim.fn.writefile(key, tls .. "/receiver-key.pem") == 0)
  local previous = nvim.env.LOUISELM_CAPTURE_STATE_DIR
  nvim.env.LOUISELM_CAPTURE_STATE_DIR = directory
  local controller = Controller.new({ service = { binary } })
  MiniTest.finally(function()
    controller:dispose()
    nvim.env.LOUISELM_CAPTURE_STATE_DIR = previous
    nvim.fn.delete(directory, "rf")
  end)
  local fixture =
    nvim.json.decode(table.concat(nvim.fn.readfile("android/app/src/test/resources/phone-control-v1.json"), "\n"))
  local proof, request = fixture.proof, fixture.request
  local receiver = "dc6c0b449b76abd2995f03d1f5d52b77658939d341d5f2533270774c29bffee3"
  proof.receiver_identity = receiver
  proof.signature =
    "3045022100860c6749778a0eb515e28106adff741eef2439caa536c173c9664d8d16f53ead02201d594a12bb89f1ab800bc66056cbad4251331bc297a7d30019674970b4ddc9c1"
  proof.signature_der, proof.signature_raw, proof.transcript = nil, nil, nil
  request.receiver_identity = receiver
  request.signature =
    "3046022100fcd132c107d5747c15a41b06f2645d616fbc113d75aaa6f48b125bf9df0f8dd80221008229de92956707de13848e7e3707df93072f9e62b05fc89eeeec998d0cb42c9f"
  request.signature_der, request.signature_raw, request.transcript, request.before, request.limit =
    nil, nil, nil, nil, nil
  local state = { id = "session-1", agent = "mock", working_dir = nvim.uv.cwd(), status = "ready" }
  local owner = { sessions = {} }
  local session = {
    state = state,
    owner = owner,
    inspect = function(self)
      return nvim.deepcopy(self.state)
    end,
  }
  owner.sessions[state.id] = session
  assert(controller:attach(request.controller_id))
  local random_subject = assert(controller:track(session))
  -- The signed fixture fixes this identity and revision; separate tests exercise random allocation.
  local subject = controller.subjects[random_subject]
  controller.subjects[random_subject] = nil
  subject.id, subject.projection.revision = request.subject_id, request.expected_revision
  controller.subjects[subject.id] = subject
  local frame = {
    v = 1,
    type = "read_session",
    controller_id = request.controller_id,
    grant = {
      grant_id = proof.grant_id,
      grant_revision = proof.grant_revision,
      enrolled_key = fixture.keys.enrolled.sec1,
      projects = { assert(nvim.uv.fs_realpath(state.working_dir)) },
      agents = { "mock" },
      groups = { "read" },
    },
    period = {
      period_id = request.period_id,
      challenge_id = proof.challenge_id,
      idle_expires_at_ms = 2000,
      absolute_expires_at_ms = 3000,
    },
    proof = proof,
    request = request,
  }
  local function read()
    local result
    controller:read(nvim.json.encode(frame), function(value)
      result = value
    end)
    assert(nvim.wait(10000, function()
      return result ~= nil
    end))
    return result
  end
  local first = read()
  assert(first.type == "session_page", nvim.inspect(first))
  request.payload = "00000000000000000015"
  MiniTest.expect.equality(read().code, "unauthenticated")
end

return T
