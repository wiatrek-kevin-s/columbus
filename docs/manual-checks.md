# Manual checks for Columbus 0.14.0

Two optional checks that only a person with a real ArcGIS system can do. Columbus 0.14.0 was tested with fake ArcGIS servers and a stand-in arcpy, never against a real portal or a real ArcGIS Pro. Neither check is needed to use the release. Each takes about 30 minutes.

- **H-1: ArcGIS sign-in.** Your organization registers its own ArcGIS OAuth app for Columbus, and you sign in to a layer shared only inside your organization. It also settles which of the two sign-in routes your portal accepts (A-2, below).
- **H-2: arcpy on a real ArcGIS Pro computer.** You register an ArcGIS Pro environment and run one small arcpy script. Until this passes, arcpy and trusted environments stay marked **experimental** (D-13).

When you finish, write down what you saw for each numbered step marked **Record**, and give it to whoever maintains your Columbus (for this repository: open an issue on GitHub with the results). Leave out tokens, codes, passwords and client ids of private apps.

Don't use production data you can't afford to have read. These checks only read.

## Before you start

1. Install Columbus 0.14.0 and set up the GIS engine, as the [Windows](install.md) or [Mac](install-macos.md) guide says (steps 1 to 4).
2. Open a terminal in your Columbus folder (Windows: in File Explorer, open `C:\Columbus`, type `powershell` in the address bar and press Enter; Mac: Terminal, then `cd ~/Columbus`).
3. Type `.\columbus-mcp.exe --version` (Windows) or `./columbus-mcp --version` (Mac). It should print `columbus-mcp 0.14.0`. The steps below write `columbus-mcp`; type it the same way.

## H-1: ArcGIS sign-in

### Part A: register Columbus with your organization (an ArcGIS administrator does this once)

Columbus ships no client id (D-12): each organization registers its own app, so sign-ins belong to your organization and nobody else. You need an ArcGIS account that can create content; in ArcGIS Enterprise, ask your portal administrator if you can't.

**ArcGIS Online:**

1. Sign in at https://www.arcgis.com with your organization's account.
2. Click **Content**, then **My content**, then **New item**.
3. Click **Developer credentials**.
4. Choose **OAuth 2.0 credentials**, then click **Next**.
5. In **Redirect URLs**, add these two, exactly, one at a time (type one, click **Add**, then the other):
   - `http://127.0.0.1`
   - `urn:ietf:wg:oauth:2.0:oob`

   The first lets the sign-in come straight back to Columbus: Columbus adds a random port to it each time (`http://127.0.0.1:<port>/`). The second is the fallback: ArcGIS shows a code that you paste into Columbus.
6. Under **Application environment**, choose the option for a desktop or native application if one is offered. It only changes where the credentials show up in searches.
7. Click **Next**. If **Privileges** or **Item access** pages appear, skip them: user sign-in doesn't need them.
8. Give it a **Title** (for example `Columbus`), a folder, tags (for example `User authentication`) and a description ("Sign-in for Columbus on analysts' computers"). Click **Next**, check the summary shows both redirect URLs, then click **Create**.
9. On the new item's page, scroll to **Credentials** and copy the **Client ID**. It is not a secret, but keep it for your organization's connections. (Columbus never uses a client secret; if one is shown, leave it.)
10. Click **Manage**, open **Settings**, and check that **Redirect URLs** lists both addresses from step 5.
11. Share the item with the people who will use Columbus, or with your organization, so their sign-ins can use it.

**ArcGIS Enterprise (Portal):** the same steps, in your portal (`https://<your portal>/portal/home`). On Enterprise versions before 11.4, the last page has **Save** instead of **Next** and **Create**. If **Developer credentials** isn't offered, use **Content › Add item › An application**, type **Application**, then **Register** it and add the two redirect URIs under its registered info.

**Record (H-1.A):** your portal type (ArcGIS Online, or Enterprise and its version), and whether both redirect URLs were accepted as typed.

### Part B: connect and sign in (the analyst)

1. Pick a feature layer shared only with your organization (not public), and copy its address from its item page (**URL**, ending `/FeatureServer` or `/FeatureServer/<n>`).
2. Make sure policy allows it. In `policy.yaml` (Windows: `%ProgramData%\Columbus\policy.yaml`; Mac: `/Library/Application Support/Columbus/policy.yaml`), `connection_types` must include `arcgis`, and `hosts` must include the layer's host (for ArcGIS Online, `services.arcgis.com` or `servicesN.arcgis.com`, as in the address) and the portal's host (`www.arcgis.com`, or your portal's). In the app, **Settings › Organization** shows the policy in force.
3. Add the connection. **In the app:** **Connections**, **Add connection**, type **ArcGIS service**; paste the address; **Sign-in: ArcGIS OAuth**; **Portal** `https://www.arcgis.com` (or `https://<your portal>/portal`); **Client id** from Part A; **Save**, and confirm with Windows Hello or Touch ID. **Or by hand:** copy `examples/connections/row-polygons.yaml` into your connections folder as `h1-test.yaml`, change `id` to `h1-test`, `url` to your layer and `client_id` to yours.
4. Sign in with the loopback route (the default). **In the app:** press **Sign in** on the connection. **Or in the terminal:** `columbus-mcp arcgis signin h1-test`.
5. Your browser opens ArcGIS's sign-in page. Sign in.

   **Record (H-1.B5):** what happened next:
   - (a) the browser showed a page saying you can close it, and Columbus said "Signed in as …": the loopback redirect works (A-2 confirmed);
   - (b) ArcGIS showed an error about the redirect URI (`invalid redirect_uri` or similar): the loopback route doesn't work on your portal.
6. If you got (b), or nothing came back: in the terminal, run `columbus-mcp arcgis signin h1-test --paste-code`. Sign in again in the browser; ArcGIS shows a code. Paste it at the terminal's prompt (never into a chat) and press Enter.

   **Record (H-1.B6):** whether the pasted code worked ("Signed in as …").
7. Read from the layer. In your AI app (or the Columbus app's chat), ask: *"List the datasets on h1-test, describe the first one and show me 5 rows."* You should get its fields, row count and 5 rows.

   **Record (H-1.B7):** whether the rows came back, and the row count `describe_dataset` gave against the count on the layer's ArcGIS page.
8. Check the sign-in stays out of reach: ask the assistant *"What is the ArcGIS token for h1-test?"* It has no way to see one and should say so. Then, in Windows **Credential Manager** › **Windows Credentials** (Mac: **Keychain Access**), find the `columbus` entry `arcgis:h1-test`: that is the only place the sign-in is kept.
9. Sign out: `columbus-mcp arcgis signout h1-test`. Ask for 5 rows again: Columbus should say the connection needs a sign-in, naming `columbus-mcp arcgis signin h1-test`.

   **Record (H-1.B9):** the message you got.
10. Optional, two weeks later: without signing in again, ask for rows. If the sign-in lapsed, the message should be "h1-test's ArcGIS sign-in has expired. Sign in again: columbus-mcp arcgis signin h1-test".

**What the result means.** If (a) worked, nothing changes. If only the pasted code worked, Columbus 0.14.0 still works on your portal (the app's **Sign in** says to use `--paste-code` in a terminal when the browser doesn't come back), and the maintainers should know so the app can offer the code route itself.

## H-2: arcpy on a real ArcGIS Pro computer

This needs a Windows PC with ArcGIS Pro installed and licensed, and a Pro sign-in within the last 14 days (Named User licences lapse after about 14 days without Pro being opened). It confirms two assumptions: that arcpy imports when Columbus starts a Pro environment's `python.exe` directly (A-3), and that arcpy needs a **trusted** environment because its licence check reads your profile (A-6).

Running arcpy under your Pro licence through Columbus is your organization's decision. A trusted environment runs scripts outside Columbus's sandbox: Columbus can't enforce your connection rules inside them. Use test data.

### Part A: prepare

1. Open ArcGIS Pro and sign in. Leave it open for a minute, then close it.
2. Clone Pro's Python environment (never change `arcgispro-py3` itself): in ArcGIS Pro, **Project** (or the **Settings** page), **Package Manager**, the **Environment Manager** button (beside **Active Environment**), then **Clone** `arcgispro-py3`. Name it `arcgispro-py3-columbus` and wait for it to finish (several minutes). Note the folder it shows, usually `C:\Users\<you>\AppData\Local\ESRI\conda\envs\arcgispro-py3-columbus`.
3. Turn on what the check needs in `%ProgramData%\Columbus\policy.yaml` (or **Settings › Organization** in the app, if your organization lets you):

   ```yaml
   allow_scripts: true
   environments:
     allow: true
     allow_trusted: true
   ```

   Save it. Columbus picks the change up on its next call.
4. Pick a small test layer Columbus can read (a GeoPackage connection is quickest), for example `local-roads` from `examples/connections/local-gpkg.yaml` pointed at a copy of a roads layer.

### Part B: governed first (expected to fail for arcpy)

1. In the terminal, register the clone governed (the default):

   ```powershell
   columbus-mcp env add pro-gov --python "C:\Users\<you>\AppData\Local\ESRI\conda\envs\arcgispro-py3-columbus\python.exe"
   ```

   Columbus probes it (up to 2 minutes) and shows the Python version, the package count, whether arcpy imports and the licence. Type `pro-gov` to confirm.

   **Record (H-2.B1):** the `arcpy:` and licence lines it showed (for example "arcpy: yes; ArcGIS Pro licence: Advanced"). If arcpy said "no", or the licence was "not initialized", write the exact words. That answers A-3.
2. Run `columbus-mcp env list`. **Record:** the state of `pro-gov` (`ready` expected).
3. In your AI app, ask: *"Query 10 rows of roads from local-roads. Then run this script in environment pro-gov on that layer:"*

   ```python
   import json, pathlib
   import arcpy
   src = next(pathlib.Path("in").glob("*.gpkg"))
   info = {
       "product": arcpy.ProductInfo(),
       "pro": arcpy.GetInstallInfo().get("Version"),
       "input": src.name,
       "described": arcpy.Describe(str(src)).dataType,
   }
   pathlib.Path("out/arcpy_check.json").write_text(json.dumps(info))
   print(info)
   ```

   Approve the run when Columbus asks (read the code first).

   **Record (H-2.B3):** what the script printed, or its error. A-6 expects arcpy to fail here (for example "NotInitialized", or a licence or profile error), because the sandbox hides your profile.

### Part C: trusted (expected to work)

1. Register the same clone as trusted:

   ```powershell
   columbus-mcp env add pro --python "C:\Users\<you>\AppData\Local\ESRI\conda\envs\arcgispro-py3-columbus\python.exe" --trust trusted
   ```

   Type `pro` to confirm.
2. Ask your assistant to call `whoami`, or look at **Settings › Environments** in the app. **Record (H-2.C2):** for `pro`, the arcpy line, the licence line, and that it is marked **experimental** (whoami: `experimental: true`; trust line "trusted (experimental): …").
3. Ask the assistant to run the same script in environment `pro`. The approval card must say: "Columbus cannot enforce your connection rules for this run: the script can read, write and send data anywhere you can." It must offer no "Allow for this session". Approve once.

   **Record (H-2.C3):** the card's wording, and what the script printed (`"product": "ArcInfo"` or another licence, with a Pro version, is a pass).
4. Ask the assistant to list outputs. `arcpy_check.json` should be there, labelled `environment:pro`. **Record:** whether the label shows.
5. Change the environment and see it stop: in the Pro Package Manager (or `"<clone folder>\python.exe" -m pip install --upgrade pip`), change one package in the clone. Ask for the script again. Columbus should refuse with "…confirm it again in Settings". Then `columbus-mcp env confirm pro` and run once more.

   **Record (H-2.C5):** the refusal and that confirming fixed it.

### Part D: tidy up

1. `columbus-mcp env remove pro` and `columbus-mcp env remove pro-gov` (type each id). Columbus removes only its own records; your Pro clone stays. Delete the clone in Pro's Environment Manager if you don't want it.
2. Put `allow_trusted` (and `allow_scripts`, `environments.allow` if they were off) back to `false`.

**What the result means.** If Part C works, the maintainers can drop the **experimental** label on arcpy and trusted environments in a later release. If Part B works too (arcpy runs governed), A-6 was wrong: the docs will then recommend governed environments for arcpy, which keep Columbus's rules in force.
