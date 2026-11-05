# LouiseLM workflow skills

These are the agent skills LouiseLM's own workflow uses. Each directory is one
skill and the exact bytes `louiselm-skills package skills/<name>` captures.

They enter a verified Session only through the usual trusted path: package,
read the Dossier, admit, witness, activate. Nothing in this directory is
trusted because it is here.

Skills here are generic: no home paths, private hosts or accounts.
`scripts/check-public-skills` enforces this in CI.

`endorsements/` holds the publisher's signed Endorsements and withdrawals for
these packages. Packaging a skill from this tree imports them automatically.
They can focus your review; they never admit anything. See
`skills-core/README.md`.

`louiselm-grill-me` is an owned fork of Matt Pocock's `grilling` skill
(<https://github.com/mattpocock/skills>, MIT); its notice ships inside the skill
as `LICENSE.upstream`, with ancestry in `UPSTREAM.md`.

`louiselm-caveman-commit` is derived from
[caveman](https://github.com/JuliusBrussee/caveman) by Julius Brussee, under the
MIT License. The full notice is in `LICENSES/caveman-MIT.txt`. It is kept outside
the skill directory so the packaged bytes match the upstream fork.
