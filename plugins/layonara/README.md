# The Layonara plugin

Moonglow's Layonara build comes with this plugin, installed and enabled
at its first start. Its commands are in **Plugins › Layonara**, and
those for an area also in an area's right-click menu.

| Command | What it does |
| --- | --- |
| **Sign In…** | Signs you in with your Layonara account (auth.layonara.com): Moonglow shows a code to enter there. Once per computer. |
| **Fetch Area…** | Brings an area from Layonara's database into the open module (by its resref, tag or part of its name). An area you have already is merged with live's changes since your copy; where both changed the same thing, you choose. |
| **From the Forge…** | Lists your own exports in the Forge (the area each becomes, when exported, the tile hak's size, and whether it was sent for review or its tiles published), and brings the one you pick into the open module: its area files, the new blueprints it uses, its tile hak and your Forge texture library (`ls_textures.hak`) into your hak folder (skipped where yours is the Forge's already), listed in the module's haks. Moonglow shows those haks over the server's NWSync content. If the module has the area already, it asks first and names what you would lose, then replaces the whole area. |
| **Try on Dev** | Puts the selected (or open) areas on the dev server, unsaved edits included, then says how to get there (`;forge`). If live's copy changed since yours, it asks: update first (the merge), try anyway on dev, or cancel. |
| **Send for Review…** | Sends the selected (or open) areas to the staff's review queue with a note of what you changed. |
| **Trial Status** | What of yours is on the dev server, and when the trial ends (1 hour without activity, 4 in all). |
| **Release…** | Puts your trial areas on dev back to live's. |
| **Sign Out** | Signs you out on this computer. |

The revision each area of your module came from is kept in the module
itself, as the local `LAYO_EXPORT_REVS` of `module.ifo` (as the
website's builder modules have it), and recorded again after each fetch
and merge. An area from the Forge also has its Forge export kept, in
the local `LAYO_FORGE` (`{area: {name, export}}`): **Send for Review**
sends it, so the review pairs your area with the Forge's Publish of its
tiles (either order). If you exported it again in the Forge since, the
review is refused: fetch it again. **Try on Dev** of a Forge area needs
its tiles on dev: press **Try on dev** in the Forge first.

It talks to `builders.layonara.com` only (Moonglow asks you before its
first request) and to `auth.layonara.com` to sign in. Publishing stays
on the website.

The service's routes: `docs/builder-api.md` in layonara-ops-api. The
tests (`mg plugin test plugins/layonara`) run against a stub of it.
