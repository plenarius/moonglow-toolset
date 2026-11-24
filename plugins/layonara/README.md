# The Layonara plugin

Moonglow's Layonara build comes with this plugin, installed and enabled
at its first start. Its commands are in **Plugins › Layonara**, and
those for an area also in an area's right-click menu.

| Command | What it does |
| --- | --- |
| **Sign In…** | Signs you in with your Layonara account (auth.layonara.com): Moonglow shows a code to enter there. Once per computer. |
| **Fetch Area…** | Brings an area from Layonara's database into the open module (by its resref, tag or part of its name). An area you have already is merged with live's changes since your copy; where both changed the same thing, you choose. |
| **Try on Dev** | Puts the selected (or open) areas on the dev server, unsaved edits included, then says how to get there (`;forge`). If live's copy changed since yours, it asks: update first (the merge), try anyway on dev, or cancel. |
| **Send for Review…** | Sends the selected (or open) areas to the staff's review queue with a note of what you changed. |
| **Trial Status** | What of yours is on the dev server, and when the trial ends (1 hour without activity, 4 in all). |
| **Release…** | Puts your trial areas on dev back to live's. |
| **Sign Out** | Signs you out on this computer. |

The revision each area of your module came from is kept in the module
itself, as the local `LAYO_EXPORT_REVS` of `module.ifo` (as the
website's builder modules have it), and recorded again after each fetch
and merge.

It talks to `builders.layonara.com` only (Moonglow asks you before its
first request) and to `auth.layonara.com` to sign in. Publishing stays
on the website.

The service's routes: `docs/builder-api.md` in layonara-ops-api. The
tests (`mg plugin test plugins/layonara`) run against a stub of it.
