# Standard apps of a BC 29.1 W1 install

This is input for deciding which first-party apps the live bc-server should load beyond System Application, Business Foundation, Base Application and APIV2. `al_model::default_app_dirs` is unchanged.

Status as of 2026-11-07.

## Source

The source is the real container `bcside` on the bc-sidebyside VM: BC 29.1.57326.57497, a W1 sandbox artifact (`Get-BcArtifactUrl -type Sandbox -country w1 -select Latest`), created without `-includeTestToolkit` (data/vm/scripts/02-bc.ps1). The list was read-only: `Get-NAVAppInfo -ServerInstance BC -Tenant default -TenantSpecificProperties` inside the container.

- 139 apps are published. 100 of them are installed in the tenant, and 39 are published only. All are Microsoft's, version 29.1.57326.57497.
- 113 are in vendor/bcapps (BCApps main, v30), matched by app id. The other 26 are the 25 language packs and "Agent Test Internal Library".
- All 113 found apps load in bc-rust today: 113 of 113 parse with 0 errors and 0 catalog warnings, loaded together with the default apps and their whole dependency closure, which is always inside BCApps. That is `al_model::Catalog::load` per app. Loading only shows that the AL parses and resolves. It doesn't run any install code, and the apps may still hit runtime gaps.
- The test framework apps (Test Runner, Any, Library Assert, ...) come with the sandbox artifact itself, since the container was created without the test toolkit.
- Manufacturing and Service Management are separate apps in 29.1, but in BCApps v30 they are empty shells (an app.json and a logo, no AL): their objects are still in the Base App.

## Apps by group

"Installed": installed in the container's tenant; "published" means published but not installed. "Extra deps": dependencies beyond the four default apps. "Load": parse errors and catalog warnings in bc-rust.

### Loaded today

| App | In tenant | BCApps path | AL files | Extra deps | Load |
|---|---|---|---:|---|---|
| _Exclude_APIV2_ | installed | `Apps/W1/APIV2/app` | 194 |  | ok |
| Application | installed | `Layers/W1/Application` | 0 |  | ok |
| Base Application | installed | `Layers/W1/BaseApp` | 8164 |  | ok |
| Business Foundation | installed | `Business Foundation/App` | 96 |  | ok |
| System Application | installed | `System Application/App` | 1345 |  | ok |

### Business functionality

| App | In tenant | BCApps path | AL files | Extra deps | Load |
|---|---|---|---:|---|---|
| API - Cross Environment Intercompany | installed | `Apps/W1/CrossEnvironmentIntercompany/app` | 21 |  | ok |
| API Reports - Finance | installed | `Apps/W1/APIReportsFinance/App` | 22 |  | ok |
| Audit File Export | installed | `Apps/W1/AuditFileExport/app` | 50 |  | ok |
| Business Central 14 Historical Data | installed | `Apps/W1/HybridBC14HistoricalData/app` | 14 |  | ok |
| Data Archive | installed | `Apps/W1/DataArchive/App` | 15 |  | ok |
| Data Search | installed | `Apps/W1/DataSearch/App` | 18 |  | ok |
| Dynamics 365 Business Central v14 Reimplementation | installed | `Apps/W1/HybridBC14/app` | 164 | Business Central 14 Historical Data, Intelligent Cloud Base | ok |
| E-Document Core | installed | `Apps/W1/EDocument/App` | 393 | PEPPOL | ok |
| Enforced Digital Vouchers | installed | `Apps/W1/EnforcedDigitalVouchers/app` | 39 | E-Document Core, PEPPOL | ok |
| Essential Business Headlines | installed | `Apps/W1/EssentialBusinessHeadlines/App` | 27 |  | ok |
| EU 3-Party Trade Purchase | installed | `Apps/W1/EU3PartyTradePurchase/app` | 25 |  | ok |
| Excise Taxes | installed | `Apps/W1/ExciseTaxes/app` | 46 | Power BI Report embeddings for Dynamics 365 Business Central, Statistical Accounts, Sustainability | ok |
| Expense Withholding Tax | installed | `Apps/W1/ExpenseWithholdingTax/app` | 11 | Expense Agent (Preview), Withholding Tax | ok |
| External Storage - Document Attachments | installed | `Apps/W1/External Storage - Document Attachments/app` | 11 |  | ok |
| Intrastat Core | installed | `Apps/W1/Intrastat/app` | 77 |  | ok |
| Manufacturing | installed | `Apps/W1/Manufacturing/app` | 0 |  | ok |
| Payment Practices | installed | `Apps/W1/PaymentPractices/App` | 40 |  | ok |
| PEPPOL | installed | `Apps/W1/PEPPOL/App` | 55 |  | ok |
| Quality Management | installed | `Apps/W1/Quality Management/app` | 219 |  | ok |
| Review General Ledger Entries | installed | `Apps/W1/ReviewGLEntries/app` | 29 |  | ok |
| SAF-T | installed | `Apps/W1/SAF-T/app` | 54 | Audit File Export | ok |
| Send remittance advice by email | installed | `Apps/W1/UKSendRemittanceAdvice/App` | 4 |  | ok |
| Service Declaration | published | `Apps/W1/ServiceDeclaration/app` | 97 |  | ok |
| Service Management | installed | `Apps/W1/ServiceManagement/app` | 0 |  | ok |
| Simplified Bank Statement Import | installed | `Apps/W1/SimplifiedBankStatementImport/App` | 23 |  | ok |
| Statistical Accounts | installed | `Apps/W1/StatisticalAccounts/app` | 44 |  | ok |
| Subcontracting | installed | `Apps/W1/Subcontracting/App` | 190 |  | ok |
| Subscription Billing | installed | `Apps/W1/Subscription Billing/App` | 374 | Power BI Report embeddings for Dynamics 365 Business Central | ok |
| Sustainability | installed | `Apps/W1/Sustainability/app` | 454 | Power BI Report embeddings for Dynamics 365 Business Central, Statistical Accounts | ok |
| Troubleshoot FA Ledger Entries | installed | `Apps/W1/DataCorrectionFA/App` | 15 |  | ok |
| VAT Group Management | installed | `Apps/W1/VATGroupManagement/app` | 51 |  | ok |
| Withholding Tax | installed | `Apps/W1/WithholdingTax/app` | 94 |  | ok |

### Base App companions (_Exclude_)

| App | In tenant | BCApps path | AL files | Extra deps | Load |
|---|---|---|---:|---|---|
| _Exclude_APIV1_ | installed | `Apps/W1/APIV1/app` | 89 |  | ok |
| _Exclude_Bank Deposits | installed | `Apps/W1/BankDeposits/app` | 52 |  | ok |
| _Exclude_Business_Events_ | installed | `Apps/W1/ExternalEvents/app` | 8 |  | ok |
| _Exclude_ClientAddIns_ | installed | `Apps/W1/ClientAddIns/app` | 1 |  | ok |
| _Exclude_Connectivity Apps | installed | `Apps/W1/ConnectivityApps/app` | 28 |  | ok |
| _Exclude_Email Logging Using Graph API | installed | `Apps/W1/EmailLogging/app` | 53 |  | ok |
| _Exclude_Master_Data_Management | installed | `Apps/W1/MasterDataManagement/app` | 55 |  | ok |
| _Exclude_Microsoft Dynamics 365 - SmartList | installed | `Apps/W1/SmartList/app` | 47 |  | ok |
| _Exclude_Onboarding Signals | installed | `Apps/W1/OnboardingSignals/app` | 6 |  | ok |
| _Exclude_PlanConfiguration_ | installed | `Apps/W1/PlanConfiguration/app` | 2 |  | ok |
| _Exclude_SyncBaseApp_ | published | `Apps/W1/SyncBase/app` | 3 |  | ok |

### Online services and connectors

| App | In tenant | BCApps path | AL files | Extra deps | Load |
|---|---|---|---:|---|---|
| Business Central Cloud Migration - Previous Release | installed | `Apps/W1/HybridBCLast/app` | 20 | Intelligent Cloud Base | ok |
| Business Central Cloud Migration API | installed | `Apps/W1/HybridAPI/app` | 6 | Intelligent Cloud Base | ok |
| Business Central Intelligent Cloud | installed | `Apps/W1/HybridBC/app` | 2 | Intelligent Cloud Base | ok |
| Business Central Mirroring to Fabric | installed | `Apps/W1/MicrosoftFabricMirroring/app` | 38 |  | ok |
| Company Hub | installed | `Apps/W1/CompanyHub/app` | 54 |  | ok |
| Dynamics BC Excel Reports | installed | `Apps/W1/ExcelReports/App` | 71 |  | ok |
| E-Document Connector - Avalara | published | `Apps/W1/EDocumentConnectors/Avalara/App` | 59 | E-Document Core, PEPPOL | ok |
| E-Document Connector - B2Brouter | published | `Apps/W1/EDocumentConnectors/B2Brouter/app` | 12 | E-Document Core, PEPPOL | ok |
| E-Document Connector - Continia | published | `Apps/W1/EDocumentConnectors/Continia/App` | 39 | E-Document Core, PEPPOL | ok |
| E-Document Connector - FORNAV | published | `Apps/W1/EDocumentConnectors/ForNAV/App` | 34 | E-Document Core, PEPPOL | ok |
| E-Document Connector - Logiq | published | `Apps/W1/EDocumentConnectors/Logiq/app` | 18 | E-Document Core, PEPPOL | ok |
| E-Document Connector - Microsoft 365 | installed | `Apps/W1/EDocumentConnectors/Microsoft365/app` | 22 | E-Document Core, PEPPOL | ok |
| E-Document Connector - Pagero | published | `Apps/W1/EDocumentConnectors/Pagero/app` | 19 | E-Document Core, PEPPOL | ok |
| E-Document Connector - SignUp | published | `Apps/W1/EDocumentConnectors/SignUp/app` | 20 | E-Document Core, PEPPOL | ok |
| Email - Current User Connector | installed | `Apps/W1/Email - Current User Connector/app` | 4 | Email - Outlook REST API | ok |
| Email - Microsoft 365 Connector | installed | `Apps/W1/Email - Microsoft 365 Connector/app` | 5 | Email - Outlook REST API | ok |
| Email - Outlook REST API | installed | `Apps/W1/Email - Outlook REST API/app` | 19 |  | ok |
| Email - SMTP API | installed | `Apps/W1/Email - SMTP API/app` | 16 |  | ok |
| Email - SMTP Connector | installed | `Apps/W1/Email - SMTP Connector/app` | 21 | Email - SMTP API | ok |
| External File Storage - Azure Blob Service Connector | installed | `Apps/W1/External File Storage - Azure Blob Service Connector/App` | 13 |  | ok |
| External File Storage - Azure File Service Connector | installed | `Apps/W1/External File Storage - Azure File Service Connector/App` | 12 |  | ok |
| External File Storage - SFTP Connector | installed | `Apps/W1/External File Storage - SFTP Connector/App` | 12 |  | ok |
| External File Storage - SharePoint Connector | installed | `Apps/W1/External File Storage - SharePoint Connector/App` | 16 |  | ok |
| Field Service Integration | published | `Apps/W1/FieldServiceIntegration/app` | 81 |  | ok |
| Intelligent Cloud Base | installed | `Apps/W1/HybridBaseDeployment/app` | 108 |  | ok |
| Microsoft Pay Payments | published | `Apps/W1/MSWalletPayments/app` | 0 |  | ok |
| Payment Links to PayPal | installed | `Apps/W1/PayPalPaymentsStandard/app` | 22 |  | ok |
| Power BI Report embeddings for Dynamics 365 Business Central | installed | `Apps/W1/PowerBIReports/App` | 282 |  | ok |
| Recommended Apps | installed | `Apps/W1/RecommendedApps/app` | 9 |  | ok |
| Send To Email Printer | installed | `Apps/W1/SendToEmailPrinter/App` | 18 |  | ok |
| Shopify Connector | installed | `Apps/W1/Shopify/App` | 514 |  | ok |
| Universal Print Integration | installed | `Apps/W1/MicrosoftUniversalPrint/app` | 24 |  | ok |

### Copilot, agents, AI/ML (Azure services)

| App | In tenant | BCApps path | AL files | Extra deps | Load |
|---|---|---|---:|---|---|
| Agent Design Experience | installed | `Apps/W1/AgentDesignExperience/app` | 58 |  | ok |
| Agent Samples | installed | `Apps/W1/AgentSamples/app` | 9 | Agent Design Experience | ok |
| AI Development Toolkit | installed | `Apps/W1/AIDevelopmentToolkit/app` | 0 | AI Development Toolkit - Design, AI Development Toolkit - Evaluation, AI Test Toolkit, Agent Design Experience, Agent Samples, Agent Samples Tests, Agent Test Library, Any, Application Test Library, Business Foundation Test Libraries, Library Assert, Library Variable Storage, Test Runner | ok |
| AI Development Toolkit - Design | installed | `Apps/W1/AIDevelopmentToolkitDesign/app` | 0 | Agent Design Experience, Agent Samples | ok |
| AI Development Toolkit - Evaluation | installed | `Apps/W1/AIDevelopmentToolkitEvaluation/app` | 0 | AI Test Toolkit, Agent Test Library, Any, Application Test Library, Business Foundation Test Libraries, Library Assert, Library Variable Storage, Test Runner | ok |
| Bank Account Reconciliation With AI | installed | `Apps/W1/BankAccRecWithAI/app` | 12 |  | ok |
| Calculate Sustainability Emission with Copilot | installed | `Apps/W1/SustainabilityCopilotSuggestion/app` | 39 | Power BI Report embeddings for Dynamics 365 Business Central, Statistical Accounts, Sustainability | ok |
| Create Product Information With Copilot | installed | `Apps/W1/CreateProductInformationWithCopilot/app` | 18 |  | ok |
| Error Messages with Recommendations | installed | `Apps/W1/ErrorMessagesWithRecommendations/App` | 13 |  | ok |
| Expense Agent (Preview) | installed | `Apps/W1/ExpenseAgent/app` | 465 |  | ok |
| Late Payment Prediction | installed | `Apps/W1/LatePaymentPredictor/app` | 31 |  | ok |
| Payables Agent | installed | `Apps/W1/PayablesAgent/app` | 76 | E-Document Connector - Microsoft 365, E-Document Core, PEPPOL | ok |
| Sales and Inventory Forecast | installed | `Apps/W1/SalesAndInventoryForecast/app` | 29 |  | ok |
| Sales Lines Suggestions | installed | `Apps/W1/SalesLinesSuggestions/app` | 44 |  | ok |
| Sales Order Agent | installed | `Apps/W1/SalesOrderAgent/app` | 106 |  | ok |

### Demo data

| App | In tenant | BCApps path | AL files | Extra deps | Load |
|---|---|---|---:|---|---|
| Contoso Coffee Demo Dataset | installed | `Apps/W1/ContosoCoffeeDemoDataset/app` | 359 |  | ok |
| E-Document Core Demo Data | installed | `Apps/W1/EDocument/Demo Data` | 18 | Contoso Coffee Demo Dataset, E-Document Core, PEPPOL | ok |
| ESG Statistical Accounts Demo Tool | installed | `Apps/W1/ESGStatisticalAccountsDemoTool/app` | 7 | Contoso Coffee Demo Dataset, Statistical Accounts | ok |
| Intrastat Core Demo Data | installed | `Apps/W1/Intrastat/demo data` | 4 | Contoso Coffee Demo Dataset, Intrastat Core | ok |
| Quality Management Contoso Coffee Demo Dataset | installed | `Apps/W1/Quality Management/Demo Data` | 12 | Contoso Coffee Demo Dataset, Quality Management | ok |
| Subscription Billing Demo Data | installed | `Apps/W1/Subscription Billing/Demo Data` | 26 | Contoso Coffee Demo Dataset, Power BI Report embeddings for Dynamics 365 Business Central, Subscription Billing | ok |
| Sustainability Contoso Coffee Demo Dataset | installed | `Apps/W1/SustainabilityContosoCoffeeDemoDataset/app` | 24 | Contoso Coffee Demo Dataset, ESG Statistical Accounts Demo Tool, Power BI Report embeddings for Dynamics 365 Business Central, Statistical Accounts, Sustainability | ok |

### Test framework

| App | In tenant | BCApps path | AL files | Extra deps | Load |
|---|---|---|---:|---|---|
| Agent Samples Tests | installed | `Apps/W1/AgentSamples/test` | 5 | AI Development Toolkit - Evaluation, AI Test Toolkit, Agent Design Experience, Agent Samples, Agent Test Library, Any, Application Test Library, Business Foundation Test Libraries, Library Assert, Library Variable Storage, Test Runner | ok |
| Agent Test Internal Library | published | not in BCApps |  |  | n/a |
| Agent Test Library | installed | `Tools/Test Framework/Test Libraries/LibraryAgent` | 7 | AI Test Toolkit, Library Assert, Test Runner | ok |
| AI Test Toolkit | installed | `Tools/AI Test Toolkit` | 71 | Test Runner | ok |
| Any | installed | `Tools/Test Framework/Test Libraries/Any` | 1 |  | ok |
| Application Test Library | installed | `Layers/W1/Tests/ApplicationTestLibrary` | 43 | Any, Business Foundation Test Libraries, Library Assert, Library Variable Storage | ok |
| Business Foundation Test Libraries | installed | `Business Foundation/Test Library` | 2 |  | ok |
| Library Assert | installed | `Tools/Test Framework/Test Libraries/Assert` | 1 |  | ok |
| Library Variable Storage | installed | `Tools/Test Framework/Test Libraries/Variable Storage` | 1 | Library Assert | ok |
| Performance Toolkit | published | `Tools/Performance Toolkit/App` | 31 | Test Runner | ok |
| Permissions Mock | published | `Tools/Test Framework/Test Libraries/Permissions Mock` | 3 |  | ok |
| Test Runner | installed | `Tools/Test Framework/Test Runner` | 50 |  | ok |

### Language packs

| App | In tenant | BCApps path | AL files | Extra deps | Load |
|---|---|---|---:|---|---|
| Czech language (Czechia) | published | not in BCApps |  |  | n/a |
| Danish language (Denmark) | published | not in BCApps |  |  | n/a |
| Dutch language (Belgium) | published | not in BCApps |  |  | n/a |
| Dutch language (Netherlands) | published | not in BCApps |  |  | n/a |
| English language (Australia) | published | not in BCApps |  |  | n/a |
| English language (Canada) | published | not in BCApps |  |  | n/a |
| English language (New Zealand) | published | not in BCApps |  |  | n/a |
| English language (United Kingdom) | published | not in BCApps |  |  | n/a |
| English language (United States) | published | not in BCApps |  |  | n/a |
| Finnish language (Finland) | published | not in BCApps |  |  | n/a |
| French language (Belgium) | published | not in BCApps |  |  | n/a |
| French language (Canada) | published | not in BCApps |  |  | n/a |
| French language (France) | published | not in BCApps |  |  | n/a |
| French language (Switzerland) | published | not in BCApps |  |  | n/a |
| German language (Austria) | published | not in BCApps |  |  | n/a |
| German language (Germany) | published | not in BCApps |  |  | n/a |
| German language (Switzerland) | published | not in BCApps |  |  | n/a |
| Icelandic language (Iceland) | published | not in BCApps |  |  | n/a |
| Italian language (Italy) | published | not in BCApps |  |  | n/a |
| Italian language (Switzerland) | published | not in BCApps |  |  | n/a |
| Norwegian language (Norway) | published | not in BCApps |  |  | n/a |
| Russian language (Russia) | published | not in BCApps |  |  | n/a |
| Spanish language (Mexico) | published | not in BCApps |  |  | n/a |
| Spanish language (Spain) | published | not in BCApps |  |  | n/a |
| Swedish language (Sweden) | published | not in BCApps |  |  | n/a |

## Notes for the decision

- PEPPOL (`Apps/W1/PEPPOL/App`, 55 files, no extra dependencies) is installed in the standard tenant and loads. E-Document Core depends on it.
- Business functionality that works offline and is installed by default: Subscription Billing (needs the Power BI embeddings app), Quality Management, Subcontracting, Statistical Accounts, Sustainability, Intrastat Core, VAT Group Management, Withholding Tax, Expense Withholding Tax, Excise Taxes, EU 3-Party Trade Purchase, Enforced Digital Vouchers, SAF-T, Audit File Export, Payment Practices, Data Archive, Data Search, Review General Ledger Entries, Troubleshoot FA Ledger Entries, Simplified Bank Statement Import, Essential Business Headlines, Send remittance advice by email, External Storage - Document Attachments and E-Document Core.
- The Copilot, agent and AI/ML apps need Azure OpenAI or Azure ML endpoints. The connectors (email, file storage, e-document providers, Shopify, PayPal, Power BI, Universal Print, cloud migration, Fabric) need their online services. Loading them is harmless, but their features need credentials and network access.
- The demo data apps (Sustainability, Quality Management, Subscription Billing, E-Document, Intrastat, ESG) add Contoso modules. They are what creates data such as Responsibility Centers, which the Contoso company lacks today (docs/demo-data.md, Known gaps).
- Language packs are not in BCApps (XLIFF translations only). bc-rust shows English captions.

## Switching to standard-prod

Status as of 2026-11-07 (P12, rounds 1–3). Trial evidence: `data/capture/standard-apps-trial-2026-11-07/` (`notes.md`, `round2/notes.md`, `round3/notes.md` and screenshots).

`bc serve` and `bc-nst` take `--apps <set>` (or `BC_APPS=<set>`). The built-in sets are in `crates/bc-server/app-sets/`:

- `standard`: the 100 apps installed in the BC 29.1 sandbox tenant above.
- `standard-prod`: `standard` without the 11 test-framework apps (Test Runner, Library Assert, Any, Library Variable Storage, Application Test Library, Business Foundation Test Libraries, AI Test Toolkit, Agent Test Library, Agent Samples Tests, and the two AI Development Toolkit apps that only depend on them). With its dependency closure that is 90 app folders: 2,426 tables and 5,006 event subscribers, against 1,857 and 3,428 for the default apps.

Without the option both servers load the default apps, as before.

### Unit changes

Append `--apps standard-prod` to both command lines and restart the units one after the other, bc-rust first:

```
# bc-rust.service
ExecStart=<repo>/target/release/bc serve --pg --host 127.0.0.1 --port 8740 --apps standard-prod
# bc-nst.service
ExecStart=<repo>/target/release/bc-nst --host 127.0.0.1 --port 7185 --apps standard-prod
```

Then `systemctl --user daemon-reload`, `systemctl --user restart bc-rust.service`, and after it logs `listening`, `systemctl --user restart bc-nst.service msweb-bcnst-8193.service`. The web server caches metadata per bc-nst start.

Take a `pg_dump` of databases `bc` and `bc_nst` first (the procedure is in the bc-rust deployment notes).

`tools/deploy/switch-standard-prod.sh` does all of this: it checks that both binaries know app sets, backs up the unit files and dumps both databases to `$BACKUP_ROOT/standard-prod-<UTC time>/` (symlink `latest`), edits the ExecStart lines, runs daemon-reload and restarts in this order, waiting for each server to listen. `--print` shows every command without running anything. `--rollback [DIR]` puts the unit files back and restarts; `--rollback --restore-data` also restores both databases from the dumps.

### What the first boot does

On every boot with a set, the server compares the loaded apps with the installed apps stored in BC's system table "NAV App Installed App" (`shared.t2000000153` in PostgreSQL). It runs before the server starts listening:

- Apps that aren't in the table are installed. Their Install codeunits run in dependency order: OnInstallAppPerDatabase, then OnInstallAppPerCompany. `DataVersion()` is 0.0.0.0 and the execution context is Install.
- Apps with an older stored version are upgraded. Their Upgrade codeunits run OnCheckPreconditions, OnUpgrade and OnValidateUpgrade, per database and then per company. `DataVersion()` is the stored version and the execution context is Upgrade.
- When the table is empty, as on the live databases today, the five default apps are recorded as installed without running their code, because bc-rust's seed created their data. `BC_INSTALL=all` runs their code too, and `BC_INSTALL=none` skips the whole step.
- Each app installs in its own transaction. If one of its triggers fails, that app's install is rolled back and it isn't recorded, and the apps that depend on it are skipped too. The log shows `NOT INSTALLED (rolled back; the next boot retries them): ...`, and the next boot tries again. This is BC's behaviour for a failed install.

For `bc` and `bc_nst` (seeded with the default apps) the first boot logs `apps: 85 installed, 0 upgraded, 4 recorded as installed (seeded), 0 unchanged, 0 failed; 42 install/upgrade triggers ran, 0 failed`. That takes about 0.6 s, before `listening`. Schema sync then adds about 570 tables and the table-extension columns in the background, which took 15 s on an empty database. Later boots log `89 unchanged` (about 30 ms).

Costs measured on the trial databases: about 0.4–0.5 GB more RSS (1.8–1.9 GB), about 0.4 s more boot time, 6.5% more AL steps per sales-order posting (13,267 against 12,461; E-Document, Subscription Billing, Sustainability and Withholding Tax subscribers run on the posting path), and about 10% more posting time.

Both servers can use one database: whichever boots first installs, the other finds the apps installed. Don't start both at the same moment on a database that still needs installs, because both would try to insert the same rows. Hence the order above.

### Rolling back

- **Back to the default apps:** remove `--apps standard-prod` from the unit and restart it. The database keeps the extra tables, columns and the "NAV App Installed App" rows; the default apps ignore them (tested: sales-order posting on a `standard-prod` database booted with the default apps, 12,461 steps). Switching back later reinstalls nothing, because the apps are still recorded as installed.
- **Back to the data before the switch:** stop the unit, drop and restore its schemas from the `pg_dump` (bc-rust deployment notes), then start it with the old command line.
- **Back to the previous binary:** the redeploy kit keeps the old binaries. A binary from before P12 ignores `--apps` (unknown options are ignored) and loads the default apps; remove the option anyway.

### Known gaps with standard-prod

- Fixed on 2026-11-08 (N1): new sales documents failed on CRONUS with "Field 'Dimension Code' does not exist in table 'Dimension Set Entry'". In standard-prod, eight object names belong to more than one object of a kind. Examples: table "Dimension Set Entry" is Base App's 480 and PowerBIReports' Removed 36950, and codeunit "Graph Client" is System Application's 9350 and the Microsoft 365 E-Document connector's 6384. The catalog registered names last-wins. It now resolves them as the AL compiler does (`crates/al-model/src/names.rs`):
  - A Removed object is never picked while a live object has the name, because valid code cannot reference it.
  - A reference in an app sees only that app and its dependencies. The file's own namespace comes first, then its `using` namespaces, then the app's own objects.
  - The context-free name map holds the object that most apps see.
  - When a reference resolves differently in its file, the catalog build pins it to the object id.
- Fixed in round 4: page 46 (sales order lines) logged `Form Lines(46): 8055 ... in table Sales Line was not loaded`. Page-extension controls bound to table-extension fields were left unbound when the catalog applied the page extension before the table extension; table and enum extensions are now applied first.
- The headline area of the Business Manager role center renders empty (not investigated; its part opens without errors since round 3).
- In `standard` (not `-prod`), Agent Samples Tests' install fails on `JsonObject.ReadFromYaml`, so it and AI Development Toolkit are not installed and are retried on each boot.
