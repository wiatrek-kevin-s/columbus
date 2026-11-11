# Manual checks for Columbus 0.15.0

Optional checks that only a person with the real systems can do. Columbus is tested with fake ArcGIS servers, a stand-in arcpy, a stand-in for the GeoLibre program, stand-in Git servers and stand-in AI services, never against a real portal, a real ArcGIS Pro, the real GeoLibre program or a real GitHub or Gitea; and no AI service was contacted while 0.15 was built, so the services new in 0.15 for the app's own chat have never met the real thing. None of these checks is needed to use Columbus. H-1 and H-2 take about 30 minutes each, H-3 and H-4 about 20, H-6 about 15, and the three short looks at the end about 5 each. H-3, H-4, H-6 and the three short looks need Columbus 0.15.0: if the newest release is still 0.14.0, they wait for 0.15.0. H-1 and H-2 can be done on 0.14.0 too.

- **H-1: ArcGIS sign-in.** Your organization registers its own ArcGIS OAuth app for Columbus, and you sign in to a layer shared only inside your organization. It also settles which of the two sign-in routes your portal accepts (A-2, below).
- **H-2: arcpy on a real ArcGIS Pro computer.** You register an ArcGIS Pro environment and run one small arcpy script. Until this passes, arcpy and trusted environments stay marked **experimental** (D-13). (The plan for 0.15 lists arcpy again as H-5: it is this same check, still open.)
- **H-3: GeoLibre.** You install the GeoLibre toolbox and have your assistant run one of its tools. This is the first real run of that program with Columbus (A-7).
- **H-4: sync with a real server.** You connect your GitHub or your organization's Gitea, link a project to a new private repository, and sync.
- **H-6: another AI service in the app's chat.** You choose an AI service other than Anthropic for the app's own chat (OpenAI, xAI, Google Gemini, OpenRouter, or a model on your own computer), save its key, and ask for a layer on the map. It is that service's first meeting with the real thing (A-11).
- **Three short looks on Windows and on a Mac:** the map panel when your assistant reads and moves the map, the choice **Work freely on copies** with Windows Hello or Touch ID, and the keys of AI services and a service added by its address, kept when the app is opened again. The app's pages are tested on Linux only.

When you finish, write down what you saw for each numbered step marked **Record**, and give it to whoever maintains your Columbus (for this repository: open an issue on GitHub with the results). Leave out tokens, keys (and the last four characters the app shows of one), codes, passwords and client ids of private apps.

Don't use production data you can't afford to have read. H-1 and H-2 only read your data. H-3 downloads and installs a program. H-4 makes a private repository on your Git server and sends two small text files to it. H-6 sends what your assistant reads to the AI service you choose, and that service bills your key for the requests.

## Before you start

1. Install Columbus and set up the GIS engine, as the [Windows](install.md) or [Mac](install-macos.md) guide says (steps 1 to 4).
2. Open a terminal in your Columbus folder (Windows: in File Explorer, open `C:\Columbus`, type `powershell` in the address bar and press Enter; Mac: Terminal, then `cd ~/Columbus`).
3. Type `.\columbus-mcp.exe --version` (Windows) or `./columbus-mcp --version` (Mac). It should print `columbus-mcp 0.15.0` (or `0.14.0`, for H-1 and H-2 on the earlier release). The steps below write `columbus-mcp`; type it the same way.

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
2. Make sure policy allows it. In `policy.yaml` (Windows: `%ProgramData%\Columbus\policy.yaml`; Mac: `/Library/Application Support/Columbus/policy.yaml`), `connection_types` must include `arcgis`, and `hosts` must include the layer's host (for ArcGIS Online, `services.arcgis.com` or `servicesN.arcgis.com`, as in the address) and the portal's host (`www.arcgis.com`, or your portal's). In the app, **Settings › Organization rules** shows the policy in force.
3. Add the connection. **In the app:** **Data**, **Add data**, type **ArcGIS service**; paste the address; **Sign-in: ArcGIS OAuth**; **Portal** `https://www.arcgis.com` (or `https://<your portal>/portal`); **Client id** from Part A; **Save**, and confirm with Windows Hello or Touch ID. **Or by hand:** copy `examples/connections/row-polygons.yaml` into your connections folder as `h1-test.yaml`, change `id` to `h1-test`, `url` to your layer and `client_id` to yours.
4. Sign in with the loopback route (the default). **In the app:** under **Data**, click the connection's row and press **Sign in** on its page. **Or in the terminal:** `columbus-mcp arcgis signin h1-test`.
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

**What the result means.** If (a) worked, nothing changes. If only the pasted code worked, Columbus still works on your portal (the app's **Sign in** says to use `--paste-code` in a terminal when the browser doesn't come back), and the maintainers should know so the app can offer the code route itself.

## H-2: arcpy on a real ArcGIS Pro computer

This needs a Windows PC with ArcGIS Pro installed and licensed, and a Pro sign-in within the last 14 days (Named User licences lapse after about 14 days without Pro being opened). It confirms two assumptions: that arcpy imports when Columbus starts a Pro environment's `python.exe` directly (A-3), and that arcpy needs a **trusted** environment because its licence check reads your profile (A-6).

Running arcpy under your Pro licence through Columbus is your organization's decision. A trusted environment runs scripts outside Columbus's sandbox: Columbus can't enforce your connection rules inside them. Use test data.

### Part A: prepare

1. Open ArcGIS Pro and sign in. Leave it open for a minute, then close it.
2. Clone Pro's Python environment (never change `arcgispro-py3` itself): in ArcGIS Pro, **Project** (or the **Settings** page), **Package Manager**, the **Environment Manager** button (beside **Active Environment**), then **Clone** `arcgispro-py3`. Name it `arcgispro-py3-columbus` and wait for it to finish (several minutes). Note the folder it shows, usually `C:\Users\<you>\AppData\Local\ESRI\conda\envs\arcgispro-py3-columbus`.
3. Turn on what the check needs in `%ProgramData%\Columbus\policy.yaml` (or **Settings › Organization rules** in the app, if your organization lets you):

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

   Approve the run if Columbus asks (read the code first). It asks unless you chose **Work freely on copies**: a governed environment runs in the sandbox.

   **Record (H-2.B3):** what the script printed, or its error. A-6 expects arcpy to fail here (for example "NotInitialized", or a licence or profile error), because the sandbox hides your profile.

### Part C: trusted (expected to work)

1. Register the same clone as trusted:

   ```powershell
   columbus-mcp env add pro --python "C:\Users\<you>\AppData\Local\ESRI\conda\envs\arcgispro-py3-columbus\python.exe" --trust trusted
   ```

   Type `pro` to confirm.
2. Ask your assistant to call `whoami`, or look at **Settings › Python environments** in the app. **Record (H-2.C2):** for `pro`, the arcpy line, the licence line, and that it is marked **experimental** (whoami: `experimental: true`; trust line "trusted (experimental): …").
3. Ask the assistant to run the same script in environment `pro`. The approval card must say: "Columbus cannot enforce your connection rules for this run: the script can read, write and send data anywhere you can." It must offer no "Allow for this session" (in the Columbus app's chat, the same choice would read "Allow this script for this chat"). Approve once.

   **Record (H-2.C3):** the card's wording, and what the script printed (`"product": "ArcInfo"` or another licence, with a Pro version, is a pass).
4. Ask the assistant to list outputs. `arcpy_check.json` should be there, labelled `environment:pro`. **Record:** whether the label shows.
5. Change the environment and see it stop: in the Pro Package Manager (or `"<clone folder>\python.exe" -m pip install --upgrade pip`), change one package in the clone. Ask for the script again. Columbus should refuse with "…confirm it again in Settings". Then `columbus-mcp env confirm pro` and run once more.

   **Record (H-2.C5):** the refusal and that confirming fixed it.

### Part D: tidy up

1. `columbus-mcp env remove pro` and `columbus-mcp env remove pro-gov` (type each id). Columbus removes only its own records; your Pro clone stays. Delete the clone in Pro's Environment Manager if you don't want it.
2. Put `allow_trusted` (and `allow_scripts`, `environments.allow` if they were off) back to `false`.

**What the result means.** If Part C works, the maintainers can drop the **experimental** label on arcpy and trusted environments in a later release. If Part B works too (arcpy runs governed), A-6 was wrong: the docs will then recommend governed environments for arcpy, which keep Columbus's rules in force.

## H-3: install GeoLibre and run a tool

The GeoLibre program brings about a thousand open-source tools. While Columbus 0.15 was built, the build machine could not run a downloaded program, so Columbus's side follows GeoLibre's source at version 1.8.1 and was tested with a stand-in (A-7). This check is the program's first real run with Columbus. It needs an internet connection for the download, and Windows 10 or 11 (64-bit, not ARM) or a Mac.

1. Turn toolboxes on. In the app, **Settings**, choose **Work freely on copies** and confirm with Windows Hello or Touch ID. (Or set `allow_toolboxes: true` in `policy.yaml` yourself.)
2. Open **Tools** and press **Add tools**. Under **GeoLibre tools** it should say version 1.8.1, licence MIT, publisher opengeos (GeoLibre), and an address under `github.com/opengeos/geolibre-rust`. Nothing is downloaded yet.
3. Press **Install** and confirm. The dialog shows each step: downloading, checking the download against the hash pinned in Columbus, unpacking, listing its tools in the sandbox.

   **Record (H-3.3):** what the dialog said at the end (how many tools are ready), and how many the Tools page says "cannot be run by Columbus yet". If it stopped, the step it stopped at and the message, word for word.
4. In the search box under **Toolboxes**, type `slope`, then `buffer`. Click one of the tools found.

   **Record (H-3.4):** whether tools were found, and whether the panel on the right showed what the tool does and its parameters.
5. In a chat, with a connection that has an elevation raster (a GeoTIFF in a projected coordinate system), ask: *"Clip a small piece of the elevation raster, then make a slope raster of it with a GeoLibre tool. Tell me which tool you ran."* No raster? Ask instead, of a layer you have (here `local-roads`, as in H-2): *"Read 100 roads from local-roads and buffer them by 50 feet with a GeoLibre tool."*

   **Record (H-3.5):** the tool's name (it starts with `geolibre.`), whether Columbus asked you to approve the run (it should not), and whether a result came back as a raster or a layer in the chat. If the tool failed, what it printed.
6. Ask for one more, on a layer you have: *"Read 100 roads from local-roads, then run a GeoLibre tool that needs a vector layer on them."* Whether GeoLibre reads every GeoPackage Columbus writes has not been tried.

   **Record (H-3.6):** whether the tool read the layer, or its error.
7. To tidy up, press **Remove** beside the toolbox on the Tools page (or keep it).

**What the result means.** If every step works, the note "never run during the build" can come out of the docs. If the install stops while listing the tools, or a tool cannot read a layer Columbus wrote, the maintainers need the exact message: Columbus's side of GeoLibre was written from its source alone.

## H-4: sync a project with a real server

Sync was built and tested against stand-in servers written from GitHub's and Gitea's published API descriptions, never against github.com, a GitHub Enterprise Server, a Gitea or a Forgejo. Use a new, empty repository that does not matter. This check works on Windows and on a Mac (on Linux no server can be connected yet).

1. Turn sync on. In the app, **Settings**, choose **Work freely on copies** and confirm. (Or set `allow_sync: true` in `policy.yaml` yourself.)
2. **Settings › Git servers › Connect a server.** Choose the kind and, unless it is github.com, give the address. Press **Open the page for a new token**, make the token there with the permissions the dialog names (on GitHub, **Contents: Read and write** and **Administration: Read and write**, so that Columbus can create the test repository), and paste it into the **Token** field (never into a chat).

   **Record (H-4.2):** the message ("Connected to … as …"), or the error, word for word. Leave the token out. A GitHub Enterprise Server is first asked what it is, without your token: if the app answers that the address "did not answer as a GitHub Enterprise Server does" although it is one, record that too, with the server's version; the maintainers need it.
3. Open **Projects**, make a project named `Sync test`, and on its page press **Open the folder**. Put a small text file in it, for example `notes.md` with one line.
4. On the project's page press **Link this project**, choose **A new private repository**, and press **Link**. If the server refuses to create it, make an empty private repository there yourself and link to it with **One of my repositories**.

   **Record (H-4.4):** which of the two worked, the message ("Linked to … No file was sent."), and that the repository on the server is private.
5. The page checks for changes by itself and should list two files as new, here: `notes.md` and `columbus-project.yaml` (the project's name and instructions). Type a note under **What did you change?** and press **Sync**.

   **Record (H-4.5):** what the page said it sent, and on the server: the two files, your note as the change's message, and how many entries the history has (on GitHub the first sync into an empty repository makes two; every later one makes one).
6. Change `notes.md` on the server, in your browser, and change it differently on your computer. Press **Check for changes**, then **Sync**. The file should be listed under **Needs a look**, with both versions kept. Choose **Keep both**.

   **Record (H-4.6):** what the page listed and the names of the two files in the folder.
7. Look where the token is kept. Windows: **Credential Manager** › **Windows Credentials**; Mac: **Keychain Access**. There should be a `columbus-app` entry named `git-server:…`. The file the page names under **Settings › Git servers** lists the server and holds no token.

   **Record (H-4.7):** that the entry is there, and that the file has no token in it.
8. Tidy up: **Disconnect** on the project's page, **Remove** the server under **Settings › Git servers**, then delete the test repository and the token on the server.

**What the result means.** If every step works, the note "tested against stand-in servers only" can come out of the docs for that kind of server. If a step fails, the maintainers need the server's kind and version and the message: the real server answered differently from its published description.

## H-6: another AI service in the app's chat

The app's own chat talks to the AI service you choose. Columbus's side of every service new in 0.15 was built from that service's published description, and every service, Anthropic too, was tested against stand-in services only; no key of any service was used (A-11). This check is the first meeting of one of them with the real thing. It works on Windows and on a Mac (on Linux no key can be saved yet). You need a connection with a small layer (here `local-roads`, as in H-2), and either a key for OpenAI, xAI, Google Gemini or OpenRouter, or Ollama or LM Studio running on your computer with a model that can call tools.

The service bills your key for these few requests. What your assistant reads in this check is sent to the service you choose, so use data you may send there.

1. Open **Settings › Assistant**. Under **Service for new chats**, choose the service, for example **OpenAI**.

   **Record (H-6.1):** the service, and the line under the list that says where its key comes from. If the list says "(not allowed by your organization)" beside it, your `policy.yaml` names other hosts under `assistant.hosts`, and the app refuses the choice with a sentence that names that rule: record the sentence and stop here.
2. Paste the key into the key box and press **Save key**. (Ollama takes no key: skip this step. LM Studio takes one only if you turned on its sign-in.)

   **Record (H-6.2):** whether the line under the box says "Key saved". Leave out the key, and the four characters the app shows of it.
3. Choose the model under **Model for new chats**. If the list is empty or lacks the one you want, press **Load the list from the service**, or type the model's id in **Another model's id**. Then press **Save settings**.

   **Record (H-6.3):** the model. If you loaded the list: what the app said ("… models listed by the service"), or the error, word for word.
4. Press **New chat**. Under the message box the chat should name the service and the model. Ask: *"Read 100 roads from local-roads and show them on my map."*

   **Record (H-6.4):** whether the steps ran (the line "… steps" above the answer), whether the panel on the right opened with the roads on the map, and the answer in a sentence. If the chat stopped, the message word for word: it names the service. If the chat said the service "refused a setting Columbus sends by choice … and took the request without it", record that line too.
5. Change the model in the chat: under the message box, choose another one (if only one is offered, skip this step). The chat should say "From here on: …" with the model's name. Then ask: *"How many roads did you read, and in which coordinate system?"*

   **Record (H-6.5):** the "From here on" line, and whether the answer came. The service under the message box should not have changed: a chat keeps its service.
6. To go back, choose your usual service under **Settings › Assistant**. To take the test key off this computer, choose the tested service once more and press **Remove key**.

**What the result means.** If every step works, the note "untried on the real thing" can come out of the docs for that service. If a step fails, the maintainers need the service, the model and the message, word for word: the real service answered differently from its published description. Small models on your own computer often call tools unreliably; a failure there may be the model's, not Columbus's, so say which model it was.

## Three short looks on Windows and on a Mac

The app's pages are tested on Linux only. These three take about five minutes each, in the Columbus app, with an AI connected for the chat.

**The map panel.** Your assistant can read what your map shows and point it at a result.

1. In a chat, ask for something that makes a layer, for example *"Read 100 roads from local-roads."* The panel on the right opens with the map.
2. Close the panel. Then ask: *"What does my map show right now?"*

   **Record:** your assistant should say that your map is not open, and the panel should stay closed.
3. Ask: *"Show those roads on my map."*

   **Record:** the panel should open with the layer on, and a line should say **Your assistant changed what the map shows.**

**The choice, with Windows Hello or Touch ID.** It needs a `policy.yaml` that is yours to change (install guide, step 3).

1. Open **Settings**. Under **How your assistant works**, choose **Work freely on copies**.

   **Record:** what asked you to confirm (Windows Hello, Touch ID or your Mac password), and the line under the two choices afterwards ("On now: Work freely on copies."). Cancel the prompt once first: nothing should change.
2. Open `policy.yaml` in a text editor. Seven settings should now be on: `allow_scripts`, `scripts.approval: none`, `environments.allow`, `allow_personal_tools`, `allow_toolboxes`, `allow_project_files` and `allow_sync`, with the rest of the file as it was.
3. Choose **Ask me before each script**. Nothing should ask you to confirm, and only `scripts.approval` should change, to `each`.

   **Record:** whether both were so.

**Keys and an added service, kept when the app is opened again.** The tests keep keys in memory and use a stand-in for the confirmation; this look uses your computer's real credential store and the real Windows Hello or Touch ID.

1. Open **Settings › Assistant**. Under **Service for new chats**, choose a second service, one you have a key for besides the one you use. Paste its key and press **Save key**. (With a key for one service only, go on to step 2.)

   **Record:** whether the line under the key box says "Key saved". Leave out the key, and the four characters the app shows of it.
2. Under **Other services**, press **Add another service**. Give it a name, such as `Test service`, and the address of a service you really have (your Azure OpenAI, your organization's gateway). With none, `https://example.org/v1` will do for this look: adding a service sends nothing to its address. Press **Add**.

   **Record:** what asked you to confirm (Windows Hello, Touch ID or your Mac password), and whether its words named the service and the host of its address. Cancel the prompt once first: nothing should be added.
3. Quit Columbus and open it again. Open **Settings › Assistant**.

   **Record:** whether the added service is still listed under **Other services**, with its address and no line in red under it; and whether each service of step 1, chosen in turn under **Service for new chats**, still says "Key saved". (On a Mac, macOS may ask for your password when the app reads a key.)
4. Tidy up: press **Remove** beside the added service, and choose your usual service again under **Service for new chats**.
