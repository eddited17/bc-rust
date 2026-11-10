// Publishes codeunit "BCR Probe Runner" as web service BCRProbes, so the runner can call
// POST /ODataV4/BCRProbes_RunProbes from inside the container. Runs on install and on every upgrade.
codeunit 50403 "BCR Probe Install"
{
    Subtype = Install;

    trigger OnInstallAppPerDatabase()
    var
        Setup: Codeunit "BCR Probe Setup";
    begin
        Setup.EnsureWebService();
    end;
}

codeunit 50404 "BCR Probe Upgrade"
{
    Subtype = Upgrade;

    trigger OnUpgradePerDatabase()
    var
        Setup: Codeunit "BCR Probe Setup";
    begin
        Setup.EnsureWebService();
    end;
}

codeunit 50405 "BCR Probe Setup"
{
    procedure EnsureWebService()
    var
        Service: Record "Tenant Web Service";
    begin
        if Service.Get(Service."Object Type"::Codeunit, 'BCRProbes') then begin
            if (Service."Object ID" = Codeunit::"BCR Probe Runner") and Service.Published then
                exit;
            Service."Object ID" := Codeunit::"BCR Probe Runner";
            Service.Published := true;
            Service.Modify();
            exit;
        end;
        Service.Init();
        Service."Object Type" := Service."Object Type"::Codeunit;
        Service."Service Name" := 'BCRProbes';
        Service."Object ID" := Codeunit::"BCR Probe Runner";
        Service.Published := true;
        Service.Insert();
    end;
}
