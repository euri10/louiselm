# louiselm.nvim

## Development

Install the pinned `mini.test` dependency from `mini.nvim`:

```sh
./scripts/install-test-deps
```

The installer uses `mini.nvim` `v0.18.0` at commit
`1345d191bb3da9c7b0e977f4387c5761f9bff68d`. Run the test suite with:

```sh
nvim --headless --noplugin -u "./tests/minimal_init.lua" -c 'lua MiniTest.run()' -c 'qa!'
```

Run one test file:

```sh
nvim --headless --noplugin -u "$pwd/tests/minimal_init.lua" \
  -c 'lua minitest.run_file("tests/schema/dsl_spec.lua")' -c 'qa!'
```

For interactive debugging, start `nvim -u ./tests/minimal_init.lua` and run
`:lua MiniTest.run()`. Without `--headless`, Neovim intentionally stays open.

To use an existing `mini.nvim` checkout instead of `.deps/mini.nvim`:

```sh
MINI_NVIM_PATH=/path/to/mini.nvim nvim --headless --noplugin \
  -u "$PWD/tests/minimal_init.lua" -c 'lua MiniTest.run()' -c 'qa!'
```
