---
name: macos-file-dialogs
description: Operate the native macOS Open and Save panels (file upload picker, File > Open, Save As), including Go to Folder, the replace prompt and hidden files.
version: 0.1.0
when:
  keywords: [save as, save dialog, open dialog, file picker, choose file, upload, attach file, go to folder, macos]
---
# macOS file dialogs

1. After the app opens a file dialog (a web "Choose file" button, **File >
    Open**, **File > Save**), run `desktop_list_apps`, then `desktop_get_state`
    on the app's window. The panel is usually a sheet attached to that window,
    sometimes a separate window, so check the current tree for it. Do not look
    for it in `browser_snapshot`. Match controls by role and label in the
    language shown on screen. Use the index and `gen` from the same current
    tree for each action.
2. To pick a location by path, press `cmd+shift+g` with `desktop_key` (pass the
    keys as a list, `["cmd","shift","g"]`). The **Go to the folder** field
    opens. Enter the absolute path (`/Users/me/Documents`, or `~/Documents`)
    with `desktop_set_value` or `desktop_type`, then press `Return`. This only
    moves the panel to that folder: nothing is opened or saved yet. If the
    shortcut is refused or no field appears, click through the sidebar and
    folder list instead.
3. **Open panel:** select the file in the list. For a file path typed into Go
    to Folder, check that the file is now highlighted. Select several files only
    when the task asks and the panel allows it: `cmd`+click adds one, `shift`+click
    selects a range. Files that are greyed out do not match the type the app
    accepts. Hidden files and folders (names starting with a dot) are not
    shown; `cmd+shift+.` toggles them, so toggle again when done. Click **Open**
    (or **Choose**), or press `Return`.
4. **Save panel:** it can be collapsed (only the **Save As** name field, **Where**
    pop-up and the buttons) or expanded (sidebar, folder list, **New Folder**).
    Click the disclosure arrow next to the name field to expand it when you need
    to pick a folder by path or create one. Replace the text in **Save As** with
    the requested file name, including the extension. Choose the folder with
    `cmd+shift+g` as above or the **Where** pop-up. If the panel has a format
    pop-up, choose the requested format before entering the name, because it can
    change the extension. Check the name, the folder and the format, then click
    **Save**.
5. If a prompt says the file "already exists. Do you want to replace it?",
    click **Replace** only when the user explicitly authorized replacing that
    exact file. Otherwise click **Cancel** (do not press `Return`), report which
    file exists, and stop until the user authorizes replacement or gives a
    different name or folder.
6. To abandon a dialog, click **Cancel** or press `Escape`. If the dialog stays
    open after you confirm, or a message appears in it, read the tree again,
    report what the app shows and stop. A permission prompt about access to a
    folder (Desktop, Documents, Downloads, a removable volume) needs the user's
    decision; do not answer it yourself.
7. Verify the result. For Save, check the requested path with `fs_list` on the
    destination folder, including the final extension. For Open or upload, go
    back to the app (for a web page, `browser_snapshot`) and check that the
    chosen file name is shown. If neither check is available, report that you
    could not verify it.

Notes:
- A panel in a sandboxed app starts in its own last-used or default folder,
    which can be iCloud Drive. Always set the folder explicitly. A file a
    sandboxed app saves with no folder chosen can end up inside its container
    under `~/Library/Containers/`, not in the folder you expected.
- Confirm with the user before uploading or opening any file they did not name.
