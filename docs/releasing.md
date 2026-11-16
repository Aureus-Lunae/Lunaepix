# Releasing a version

How a version `X.Y.Z` goes from `development` to users. `main` is for
releases only. `development` is the branch the work happens on.

The Homebrew formula (`Formula/lunaepix.rb`) points at the tag's source
archive and carries that archive's checksum. The checksum is of the tagged
commit itself, so it cannot be inside that commit. It goes in by one commit
right after the tag. Until that commit is pushed, `brew install` fails with a
checksum mismatch. That takes minutes, so do the steps below in one go.

Before you start, `development` has the version `X.Y.Z` in the workspace
`Cargo.toml`, and the formula's `url` names `vX.Y.Z` (a test checks that they
match). The formula's `sha256` is still 64 zeros.

## Checklist

1. Merge `development` into `main`, keeping a merge commit:

   ```
   git switch main
   git merge --no-ff development
   ```

2. Tag the merge commit and push `main` and the tag:

   ```
   git tag -a vX.Y.Z -m "Lunaepix X.Y.Z"
   git push origin main vX.Y.Z
   ```

   The tag starts the Windows and Mac workflows.

3. Get the checksum of the tag's source archive. Either download it and
   check the file:

   ```
   curl -L -o vX.Y.Z.tar.gz https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/vX.Y.Z.tar.gz
   shasum -a 256 vX.Y.Z.tar.gz
   ```

   or check it as it comes in:

   ```
   curl -L https://github.com/Aureus-Lunae/Lunaepix/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256
   ```

4. Put the value into `Formula/lunaepix.rb`, on the `sha256` line (the 64
   hex digits, nothing else). Leave the `url` as it is.

5. Commit on `main` and push:

   ```
   git add Formula/lunaepix.rb
   git commit -m "Formula: X.Y.Z"
   git push origin main
   ```

   From this push on, `brew install` works for `X.Y.Z`.

6. Merge `main` back into `development` and push, so `development` has the
   checksum too:

   ```
   git switch development
   git merge main
   git push origin development
   ```

## The next version

Every built change raises the workspace version. On `development`, at the
next version bump (`X.Y.Z` to the next one), change the formula's `url` to
the next tag and set `sha256` back to 64 zeros. The test that compares the
formula's `url` with the workspace version fails until you do. The real
checksum for that version comes in with its own release.

## If a step goes wrong

- A checksum mismatch from `brew install` after step 5 means the value in the
  formula is not the tag's archive. Run step 3 again and compare.
- Do not move a tag that is already pushed: its archive, and so its checksum,
  would change. Make a new version instead.
