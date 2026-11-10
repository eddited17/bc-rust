// Entry point of a probe run. Args: '<run id>|<probe id>,<probe id>,...|<context>;<context>;...'
// A context is '&'-separated key=value pairs, e.g. 'lang=1031'. Supported keys:
//   lang: GlobalLanguage for the probes in this context (a Windows language id, e.g. 1033, 1031).
// Every probe runs once per context; each line carries the context as "ctx".
codeunit 50401 "BCR Probe Runner"
{
    procedure RunProbes(Args: Text): Text
    var
        Log: Codeunit "BCR Probe Log";
        Registry: Codeunit "BCR Probe Registry";
        Parts: List of [Text];
        Ids: List of [Text];
        Contexts: List of [Text];
        Id: Text;
        Sel: Text;
        CtxText: Text;
        Ctx: JsonObject;
        RunId: Code[50];
        StartLang: Integer;
        Runs: Integer;
    begin
        Parts := Args.Split('|');
        RunId := CopyStr(Parts.Get(1), 1, MaxStrLen(RunId));
        Ids := Parts.Get(2).Split(',');
        if Parts.Count() >= 3 then
            Contexts := Parts.Get(3).Split(';');
        if Contexts.Count() = 0 then
            Contexts.Add('');
        StartLang := GlobalLanguage();
        Log.Init(RunId);
        foreach CtxText in Contexts do begin
            Ctx := ApplyContext(CtxText);
            Log.SetContext(Ctx);
            foreach Id in Ids do begin
                // 'id:step+step' runs only those steps of a hand-written probe
                Sel := '';
                if StrPos(Id, ':') > 0 then begin
                    Sel := CopyStr(Id, StrPos(Id, ':') + 1);
                    Id := CopyStr(Id, 1, StrPos(Id, ':') - 1);
                end;
                Log.SetSelector(Sel);
                if not Registry.Run(Id, Log) then begin
                    Log.BeginProbe(CopyStr(Id, 1, 100), 0);
                    Log.Note('unknown probe id');
                end;
                Runs += 1;
                Log.Flush();
            end;
            GlobalLanguage(StartLang);
        end;
        Log.Finish();
        exit(StrSubstNo('run %1: %2 probe runs', RunId, Runs));
    end;

    // Same arguments as RunProbes, but the run happens in a background session (codeunit "BCR Probe Bg"); the
    // request row "BCR Probe Request" tracks it. Returns at once.
    procedure RunProbesBackground(Args: Text): Text
    var
        Req: Record "BCR Probe Request";
        Parts: List of [Text];
        NewSession: Integer;
        Timeout: Duration;
    begin
        Parts := Args.Split('|');
        Req.Init();
        Req."Run Id" := CopyStr(Parts.Get(1), 1, MaxStrLen(Req."Run Id"));
        Req.Args := CopyStr(Args, 1, MaxStrLen(Req.Args));
        Req.Status := 'Queued';
        Req."Created At" := CurrentDateTime();
        Req.Insert();
        Commit();
        Timeout := 4 * 60 * 60 * 1000;
        if not StartSession(NewSession, Codeunit::"BCR Probe Bg", CompanyName(), Req, Timeout) then begin
            Req.Status := 'Failed';
            Req.Message := 'StartSession returned false';
            Req.Modify();
            exit(StrSubstNo('run %1: StartSession failed', Req."Run Id"));
        end;
        exit(StrSubstNo('run %1: background session %2', Req."Run Id", NewSession));
    end;

    local procedure ApplyContext(CtxText: Text) Ctx: JsonObject
    var
        Pair: Text;
        Name: Text;
        Value: Text;
        Lang: Integer;
        P: Integer;
    begin
        if CtxText = '' then
            exit;
        foreach Pair in CtxText.Split('&') do begin
            P := StrPos(Pair, '=');
            Name := CopyStr(Pair, 1, P - 1);
            Value := CopyStr(Pair, P + 1);
            case Name of
                'lang':
                    begin
                        Evaluate(Lang, Value);
                        GlobalLanguage(Lang);
                        Ctx.Add('lang', Lang);
                    end;
                else
                    Ctx.Add(Name, Value);
            end;
        end;
    end;
}
