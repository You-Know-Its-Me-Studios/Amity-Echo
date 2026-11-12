# Amity Echo for VS Code

This extension sends the active file path, selected text, and current diagnostics to an Amity Echo one-shot request. It shows Echo's progress and answer in the **Amity Echo** output channel. File changes and commands still pass through the connected Amity project.

Install the `amity-echo-editor.vsix` asset from the matching [Echo release](https://github.com/You-Know-Its-Me-Studios/Amity-Echo/releases) with **Extensions → Install from VSIX**, or run `code --install-extension amity-echo-editor.vsix`. Install `echocli` on your PATH and connect the project in Echo first. Then right-click a file in the editor and choose **Amity Echo: Ask About Selection**.

The extension runs Echo locally. Pending approvals and policy decisions are reviewed in the Echo terminal in the same project. Editor context is sent for one request and is not stored as a session attachment.
