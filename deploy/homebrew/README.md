# Publishing this formula

`the-cybersapien/homebrew-lumberroom` does not exist yet. `lumberroom.rb` lives here so it can be
reviewed alongside the release it targets, then copied into that tap once the tag is cut.

## First release

1. Cut tag `v0.1.0` and let the release job publish the four archives and `SHA256SUMS` at
   `https://github.com/the-cybersapien/lumberroom/releases/download/v0.1.0/`.
2. Read the four hashes out of `SHA256SUMS` and replace the four placeholder `sha256` values in
   `lumberroom.rb`. Each placeholder is commented with which archive it belongs to.
3. Create the tap repository: `gh repo create the-cybersapien/homebrew-lumberroom --public`.
4. Clone it, create `Formula/lumberroom.rb`, copy this file's contents in, commit, and push.
5. Verify from a clean machine:
   ```
   brew tap the-cybersapien/lumberroom
   brew install lumberroom
   brew test lumberroom
   ```

## Later releases

Bump `version`, update the four URLs and `sha256` values, commit to the tap. `brew bump-formula-pr`
can do steps 2-4 against the tap repository once it exists.
