-- syncbot's build, as recipes. This replaced the Makefile; there is no other.
--
--   make            the recipes, with what each of them says it does
--   make build      the library
--   make test       the suite
--   make ci         everything that gates a merge (run before pushing)
--
-- At an oslo prompt in this directory `make` is enough; everywhere else it is `oslo make`.
-- The dev shell's toolchain comes from `.env.lua`'s `nix_develop()`, so recipes call `cargo`
-- directly rather than wrapping every command in `nix develop -c`.

local make = oslo.make

local FUZZ_TARGETS = { "workspace_push", "canonical_datapod" }

-- name = ... from Cargo.toml — the one place every tool reads it from.
local function project_name()
  local content = oslo.fs.read("Cargo.toml") or ""
  local name = content:match('\nname%s*=%s*"([^"]+)"') or content:match('^name%s*=%s*"([^"]+)"')
  assert(name, "Cargo.toml package name not found or invalid")
  return name
end

local function need(tool, why)
  assert(oslo.run{ "sh", "-c", "command -v " .. tool, capture = true }.ok, why)
end

local NAME = project_name()

make.recipe{ name = "build", desc = "the library",
             run = function() sh.cargo("build", "--lib") end }
make.alias("b", "build")

make.recipe{ name = "compile", desc = "clean, then build", deps = { "clean", "build" } }
make.alias("c", "compile")

make.recipe{ name = "test", desc = "run all tests",
             run = function() sh.cargo("test", "--all-targets") end }
make.alias("t", "test")

make.recipe{ name = "test-peerbus", desc = "test the canonical peerbus core/client",
             run = function() sh.cargo("test", "--all-targets", "--features", "peerbus") end }

make.recipe{ name = "test-all", desc = "test all transport adapters",
             run = function() sh.cargo("test", "--all-targets", "--features", "peerbus rest xmlt") end }

make.recipe{ name = "check", desc = "cargo check on all targets",
             run = function() sh.cargo("check", "--all-targets") end }

make.recipe{ name = "check-peerbus", desc = "check the canonical peerbus core/client",
             run = function() sh.cargo("check", "--all-targets", "--features", "peerbus") end }

make.recipe{ name = "check-all", desc = "check all transport adapters",
             run = function() sh.cargo("check", "--all-targets", "--features", "peerbus rest xmlt") end }

make.recipe{ name = "fmt", desc = "format the workspace",
             run = function() sh.cargo("fmt", "--package", NAME) end }

make.recipe{ name = "clean", desc = "remove Cargo build artifacts",
             run = function() sh.cargo("clean") end }

-- Everything that would gate a merge, in one command. Codeberg Actions is not
-- enabled for this repository, so this is the gate — run it before pushing.
make.recipe{
  name = "ci",
  desc = "everything that gates a merge (run before pushing)",
  run = function()
    print("== check (all adapters)")
    sh.cargo("check", "--all-targets", "--features", "peerbus rest xmlt")
    print("== check (python bindings)")
    sh.cargo("check", "--features", "python")
    print("== clippy")
    sh.cargo("clippy", "--all-targets", "--features", "peerbus rest xmlt", "--", "-D", "warnings")
    print("== fmt")
    sh.cargo("fmt", "--package", NAME, "--", "--check")
    print("== tests")
    sh.cargo("test", "--all-targets", "--features", "peerbus rest xmlt")
    print("")
    print("ci: all green")
  end,
}

-- cargo-fuzz needs nightly, which the default dev shell deliberately does not
-- provide; flake.nix carries a `fuzz` shell for it. Corpora persist under fuzz/corpus/.
make.recipe{
  name = "fuzz",
  desc = "fuzz one target (--target=, --seconds=)",
  params = {
    { "--target", desc = "which fuzz target", default = "workspace_push" },
    { "--seconds", desc = "how long to run", default = "60" },
  },
  run = function(a)
    local target = a.target or "workspace_push"
    local seconds = a.seconds or "60"
    assert(oslo.run{
      "nix", "develop", ".#fuzz", "--command", "bash", "-c",
      ("cd fuzz && cargo fuzz run %s -- -max_total_time=%s -rss_limit_mb=4096"):format(target, seconds),
    }.ok, "fuzz failed")
  end,
}

make.recipe{
  name = "fuzz-all",
  desc = "fuzz every target in turn",
  params = { { "--seconds", desc = "how long each target runs", default = "60" } },
  run = function(a)
    local seconds = a.seconds or "60"
    for _, target in ipairs(FUZZ_TARGETS) do
      print(("=== fuzzing %s for %ss"):format(target, seconds))
      assert(oslo.run{
        "nix", "develop", ".#fuzz", "--command", "bash", "-c",
        ("cd fuzz && cargo fuzz run %s -- -max_total_time=%s -rss_limit_mb=4096"):format(target, seconds),
      }.ok, target .. " failed")
    end
  end,
}

make.recipe{ name = "bind", desc = "generate both C and Python bindings",
             deps = { "bind-c", "bind-py" } }

make.recipe{
  name = "bind-c",
  desc = "generate the C header",
  run = function()
    sh.cargo("build", "--lib")
    sh.cbindgen("--config", "cbindgen.toml", "--crate", NAME, "--output", "include/" .. NAME .. ".h")
  end,
}

-- Built as a wheel and unpacked into target/python, which .env.lua puts on
-- PYTHONPATH — no venv, nothing installed.
make.recipe{
  name = "bind-py",
  desc = "generate the Python bindings",
  run = function()
    oslo.run{ "rm", "-rf", "target/python", "target/wheels" }
    sh.maturin("build", "--quiet", "--features", "python", "--out", "target/wheels")
    local wheel = oslo.fs.glob("target/wheels/*.whl")[1]
    assert(wheel, "no wheel found in target/wheels")
    sh.python3("-m", "zipfile", "-e", wheel, "target/python")
  end,
}

make.recipe{
  name = "release",
  desc = "cut a version: --type patch | minor | major | M.m.p",
  params = { { "--type", desc = "patch | minor | major | M.m.p" } },
  run = function(a)
    need("git-rel", "git-rel is not installed; install it first")
    assert(type(a.type) == "string",
           "which release? make release --type patch|minor|major|M.m.p")
    sh.git("rel", a.type)
  end,
}
