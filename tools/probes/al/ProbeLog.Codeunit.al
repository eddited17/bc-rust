// Collects one JSON line per probe cell and writes the lines to table "BCR Probe Line" in chunks.
// Line shapes (keys in this order):
//   meta: {"probe", "ctx", "meta": {session facts}}
//   cell: {"probe", "ctx", "cell", "in", "ok", "result", "error", "code"[, "out"]}
//   raw:  {"probe", "ctx", "cell", "raw": <one line or array element of a RunAll-style probe>}
//   note: {"probe", "ctx", "note"}
//   end:  {"done": true, "run", "lines"} (in its own chunk, "Probe Id" = '#done')
// Flush commits, so call it only between cells (the generated drivers and the runner do).
codeunit 50400 "BCR Probe Log"
{
    var
        Buf: List of [Text];
        Ctx: JsonObject;
        RunId: Code[50];
        ProbeId: Text[100];
        Chunk: Integer;
        Cells: Integer;
        Total: Integer;
        ChunkSize: Integer;
        Selector: Text;
        FileSeq: Integer;
        PendingFiles: JsonArray;

    procedure Init(NewRunId: Code[50])
    begin
        RunId := NewRunId;
        Chunk := 0;
        Total := 0;
        ChunkSize := 2000;
        FileSeq := 0;
        Clear(Buf);
        Clear(Ctx);
        Clear(PendingFiles);
    end;

    // Step selector of the current probe ('' = all steps; else '+'-separated step names).
    procedure SetSelector(NewSelector: Text)
    begin
        Selector := NewSelector;
    end;

    // True when StepName is selected: no selector, an exact name, or a name ending in * that is a prefix.
    procedure Selected(StepName: Text): Boolean
    var
        Item: Text;
    begin
        if Selector = '' then
            exit(true);
        foreach Item in Selector.Split('+') do
            if Item.EndsWith('*') then begin
                if StepName.StartsWith(Item.TrimEnd('*')) then
                    exit(true);
            end else
                if StepName = Item then
                    exit(true);
        exit(false);
    end;

    procedure SetContext(NewCtx: JsonObject)
    begin
        Ctx := NewCtx;
    end;

    procedure BeginProbe(NewProbeId: Text[100]; CellCount: Integer)
    var
        L: JsonObject;
    begin
        Flush();
        ProbeId := NewProbeId;
        Cells := 0;
        L.Add('probe', ProbeId);
        L.Add('ctx', Ctx.Clone());
        L.Add('meta', SessionFacts(CellCount));
        AddLine(L);
    end;

    // For hand-written probes: logs one cell and takes the error text and code from GetLastErrorText and
    // GetLastErrorCode (call it right after the TryFunction or Codeunit.Run that ran the cell), then clears them.
    procedure Cell(Input: JsonObject; Ok: Boolean; Result: Text)
    var
        NoOut: JsonObject;
    begin
        CellFull(Cells + 1, Input, Ok, Result, GetLastErrorText(), GetLastErrorCode(), NoOut);
        ClearLastError();
    end;

    procedure CellOut(Input: JsonObject; Ok: Boolean; Result: Text; Out: JsonObject)
    begin
        CellFull(Cells + 1, Input, Ok, Result, GetLastErrorText(), GetLastErrorCode(), Out);
        ClearLastError();
    end;

    // CellNo: the cell's index in the probe's grid (generated probes) or a running number (hand-written probes).
    procedure CellFull(CellNo: Integer; Input: JsonObject; Ok: Boolean; Result: Text; ErrText: Text; ErrCode: Text; Out: JsonObject)
    var
        L: JsonObject;
    begin
        Cells := CellNo;
        L.Add('probe', ProbeId);
        L.Add('ctx', Ctx.Clone());
        L.Add('cell', CellNo);
        L.Add('in', Input.Clone());
        L.Add('ok', Ok);
        L.Add('result', Result);
        L.Add('error', ErrText);
        L.Add('code', ErrCode);
        if Out.Keys().Count() > 0 then
            L.Add('out', Out.Clone());
        if PendingFiles.Count() > 0 then begin
            L.Add('files', PendingFiles.Clone());
            Clear(PendingFiles);
        end;
        AddLine(L);
    end;

    // Stores a binary output of the next cell (e.g. a workbook from Report.SaveAs) in table "BCR Probe File" and
    // returns its file name '<cell>-<Name>'; the next Cell/CellOut/CellFull line lists it under "files". The runner
    // writes it to evidence/<probe>/<ts>.files/. Commits: call it outside TryFunctions and outside cells.
    procedure AddFile(Name: Text; var Source: InStream): Text
    var
        F: Record "BCR Probe File";
        OS: OutStream;
        FileName: Text;
    begin
        FileSeq += 1;
        FileName := CopyStr(StrSubstNo('%1-%2', Cells + 1, Name), 1, MaxStrLen(F.Name));
        F.Init();
        F."Run Id" := RunId;
        F.Seq := FileSeq;
        F."Probe Id" := ProbeId;
        F.Cell := Cells + 1;
        F.Name := CopyStr(FileName, 1, MaxStrLen(F.Name));
        F.Data.CreateOutStream(OS);
        CopyStream(OS, Source);
        F.Insert();
        F.CalcFields(Data);
        F.Size := F.Data.Length();
        F.Modify();
        Commit();
        PendingFiles.Add(FileName);
        exit(FileName);
    end;

    procedure Note(Message: Text)
    var
        L: JsonObject;
    begin
        L.Add('probe', ProbeId);
        L.Add('ctx', Ctx.Clone());
        L.Add('note', Message);
        AddLine(L);
    end;

    // For probes with the RunAll(): Text contract: one JSON object per line (other lines are kept as text).
    procedure RawText(Lines: Text)
    var
        LF: Text[1];
        Line: Text;
    begin
        LF[1] := 10;
        foreach Line in Lines.Split(LF) do
            if Line.TrimEnd() <> '' then
                RawLine(Line.TrimEnd());
    end;

    procedure RawLine(Line: Text)
    var
        L: JsonObject;
        O: JsonObject;
    begin
        Cells += 1;
        L.Add('probe', ProbeId);
        L.Add('ctx', Ctx.Clone());
        L.Add('cell', Cells);
        if O.ReadFrom(Line) then
            L.Add('raw', O)
        else
            L.Add('rawText', Line);
        AddLine(L);
    end;

    // For probes with the RunAll(var Results: JsonArray) contract.
    procedure RawArray(Results: JsonArray)
    var
        T: JsonToken;
        L: JsonObject;
    begin
        foreach T in Results do begin
            Clear(L);
            Cells += 1;
            L.Add('probe', ProbeId);
            L.Add('ctx', Ctx.Clone());
            L.Add('cell', Cells);
            L.Add('raw', T.Clone());
            AddLine(L);
        end;
    end;

    procedure Finish()
    var
        L: JsonObject;
    begin
        Flush();
        ProbeId := '#done';
        L.Add('done', true);
        L.Add('run', RunId);
        L.Add('lines', Total + 1);
        AddLine(L);
        Flush();
    end;

    procedure Flush()
    var
        Line: Record "BCR Probe Line";
        OS: OutStream;
        T: Text;
        LF: Text[1];
    begin
        if Buf.Count() = 0 then
            exit;
        LF[1] := 10;
        Chunk += 1;
        Line.Init();
        Line."Run Id" := RunId;
        Line.Chunk := Chunk;
        Line."Probe Id" := ProbeId;
        Line.Lines := Buf.Count();
        Line."Created At" := CurrentDateTime();
        Line.Data.CreateOutStream(OS, TextEncoding::UTF8);
        foreach T in Buf do begin
            OS.WriteText(T);
            OS.WriteText(LF);
        end;
        Line.Insert();
        Commit();
        Clear(Buf);
    end;

    local procedure AddLine(L: JsonObject)
    var
        T: Text;
    begin
        L.WriteTo(T);
        Buf.Add(T);
        Total += 1;
        if Buf.Count() >= ChunkSize then
            Flush();
    end;

    local procedure SessionFacts(CellCount: Integer) M: JsonObject
    begin
        M.Add('run', RunId);
        M.Add('cells', CellCount);
        M.Add('user', UserId());
        M.Add('company', CompanyName());
        M.Add('globalLanguage', GlobalLanguage());
        M.Add('windowsLanguage', WindowsLanguage());
        M.Add('clientType', Format(CurrentClientType()));
        M.Add('sessionId', SessionId());
        M.Add('formatDecimal', Format(1234567.891));
        M.Add('formatDate', Format(20261231D));
        M.Add('formatTime', Format(134501T));
        M.Add('formatDateTime', Format(CreateDateTime(20261231D, 134501T)));
        M.Add('startedUtc', Format(CurrentDateTime(), 0, 9));
    end;
}
