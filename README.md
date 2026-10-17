# SimpleNotes

Your notes and to-dos, always within reach on your desktop.

SimpleNotes is inspired by Windows Sticky Notes. It lets you keep reminders on your desktop, behind your other windows, without taking up space on the taskbar. Everything is managed through a small icon next to the clock.
<img width="591" height="509" alt="image" src="https://github.com/user-attachments/assets/3f48f816-0ea7-4edd-a03c-36fa82ae5e9a" />

## Getting started

1. Open `SimpleNotes.exe` inside the `dist` folder.
2. Find the yellow icon next to the clock. If you cannot see it, click the **^** arrow to show hidden icons.
3. Left-click or right-click the icon and choose **New note**.
4. Start typing. Your changes are saved automatically.

You can drag the icon out of the hidden icons menu if you prefer to keep it visible. No account or Internet connection is required.

## A desktop with fewer distractions

- **Notes stay in the background:** they remain on your desktop, behind other applications. Keep that part of the desktop visible to read or edit them.
- **A clear taskbar:** notes do not add taskbar buttons or appear when switching windows with Alt+Tab.
- **Discreet controls:** they appear when you hover over a note and fade away when you move the pointer away.
- **Dark theme:** notes, settings, My notes, menus, and messages share the same dark appearance.
- **One running instance:** opening SimpleNotes again brings up the settings of the instance that is already running.

## Organizing your notes

Drag an empty area of the header to move a note. Drag an edge or corner to resize it.

Hover over a note to reveal these controls:

| Control | What it does |
| --- | --- |
| **×**, in the upper-left corner | Archives the note to remove it from your desktop. |
| **+** | Creates a new note. |
| **···** | Opens the note's options. You can also right-click. |

**Archiving does not delete your note.** Open **My notes** from the icon next to the clock, select an archived note, and click **Open / restore** to return it to your desktop.

To temporarily clear your desktop, choose **Hide notes** from the icon's menu. **Show notes** brings them back.

## Turning a note into a to-do list

Start the note with a dash and write one task per line:

```text
- Prepare the study guide
- Study Operating Systems
- Review the project
```

Open **···** or right-click and choose **Enable ToDoList**. A single task is enough, and you can also omit the space after the dash, as in `-Prepare the study guide`.

Each task has a circle. Click it to change its status:

| Color | Status |
| --- | --- |
| 🔴 Red | Pending |
| 🟡 Yellow | In progress |
| 🟢 Green | Done |

Clicking again after green returns the task to pending.

- **Add a task:** type in the field at the bottom and press **Enter** or **+**.
- **Edit its name:** double-click the task's text. Make your changes in the field at the bottom, then press **Enter** or **✓**.
- **Finish the list:** once every task is done, SimpleNotes asks whether you want to archive the list. You can accept or leave it on your desktop.
- **Return to a text note:** choose **Disable ToDoList** from its menu. Your tasks are preserved. If you enable it again without changing that text, their previous statuses are restored.

Anything you are typing in the field at the bottom is also saved, even if you have not added the task yet.

## Making SimpleNotes your own

Open **Settings** from the icon's menu next to the clock:

- **Start with Windows:** makes your notes available when you sign in.
- **Scale:** move the slider to enlarge or shrink the notes' text and controls. Changes take effect immediately.

If you move the executable and use automatic startup, turn that option off and back on from the new location.

## Your notes are saved as you type

You can exit through the icon's menu and reopen SimpleNotes to continue with your notes and tasks.

If the application closes unexpectedly or you force it to quit, changes that have finished saving are preserved. If this happens during a write, that last change could be lost.

If you see **Not saved · retrying**, the application is having trouble saving. Keep the note open and copy any text you need to preserve while it retries.

From the **Backup** menu, you can:

- **Create a backup** of your notes and tasks.
- **Export notes to text files**, including archived notes.
- **Open the data folder** to find your backups and exports.

SimpleNotes also creates daily backups and keeps up to 30. To protect your notes against a drive failure, you can keep a copy on another device.

## Useful shortcuts

| Shortcut | Action |
| --- | --- |
| **Ctrl+N** | Create a note. |
| **Ctrl+,** | Open settings. |
| **Ctrl+A** | Select all text. |
| **Ctrl+S** | Retry saving. |
| **Alt+F4** on a note | Archive it. |
| **Space** on a selected task | Change its status. |
| **F2** on a selected task | Edit its name. |
| **Tab** in a to-do list | Move between the list, the text field, and the button. |

You can also copy, paste, and undo using the usual Windows shortcuts.
