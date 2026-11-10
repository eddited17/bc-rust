// Background runs: RunProbesBackground (web service action) stores the request and starts a background session,
// so report rendering (Report.SaveAs) never runs inside a web service request. The runner polls the request's
// Status with SQL ('Queued', 'Running', 'Done', 'Failed').
table 50401 "BCR Probe Request"
{
    Caption = 'BCR Probe Request';
    DataClassification = SystemMetadata;
    DataPerCompany = false;

    fields
    {
        field(1; "Run Id"; Code[50]) { }
        field(2; Args; Text[2048]) { }
        field(3; Status; Text[30]) { }
        field(4; Message; Text[2048]) { }
        field(5; "Session Id"; Integer) { }
        field(6; "Created At"; DateTime) { }
        field(7; "Finished At"; DateTime) { }
    }

    keys
    {
        key(PK; "Run Id") { Clustered = true; }
    }
}

codeunit 50406 "BCR Probe Bg"
{
    TableNo = "BCR Probe Request";

    trigger OnRun()
    var
        Req: Record "BCR Probe Request";
        Ok: Boolean;
    begin
        Req.Get(Rec."Run Id");
        Req.Status := 'Running';
        Req."Session Id" := SessionId();
        Req.Modify();
        Commit();
        ClearLastError();
        Ok := Codeunit.Run(Codeunit::"BCR Probe Bg Inner", Req);
        Req.Get(Rec."Run Id");
        if Ok then
            Req.Status := 'Done'
        else begin
            Req.Status := 'Failed';
            Req.Message := CopyStr(GetLastErrorCode() + ': ' + GetLastErrorText(), 1, MaxStrLen(Req.Message));
        end;
        Req."Finished At" := CurrentDateTime();
        Req.Modify();
        Commit();
    end;
}

codeunit 50407 "BCR Probe Bg Inner"
{
    TableNo = "BCR Probe Request";

    trigger OnRun()
    var
        Runner: Codeunit "BCR Probe Runner";
    begin
        Runner.RunProbes(Rec.Args);
    end;
}
