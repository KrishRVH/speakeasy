module

public import Speakeasy.Spec.Basic

/-!
# Recognition model

`machine::recognition::Recognition`: which part of a session's audio goes to the worker next,
and which results it still accepts. Pieces and streams transcribe while recording; the final
part covers the tail after the pieces, or the whole recording when the pieces cannot be trusted.
-/

@[expose] public section

namespace Speakeasy

/-- A live stream's progress. -/
inductive Stream where
  /-- Waiting to be sent. -/
  | queued
  /-- At the worker. -/
  | sent
  /-- Heard. -/
  | heard
  deriving DecidableEq

/-- Decides whether two stream values are equal. -/
add_decl_doc instDecidableEqStream

/-- A stream value is the constructor its index names. -/
add_decl_doc Stream.ofNat_ctorIdx

/-- One piece's progress. -/
inductive Piece where
  /-- Waiting to be sent. -/
  | queued
  /-- At the worker. -/
  | sent
  /-- Heard. -/
  | heard
  deriving DecidableEq

/-- Decides whether two piece values are equal. -/
add_decl_doc instDecidableEqPiece

/-- A piece value is the constructor its index names. -/
add_decl_doc Piece.ofNat_ctorIdx

/-- What recognition does before the final part. -/
inductive Early where
  /-- Transcribe pieces in order. -/
  | segmented (pieces : List Piece)
  /-- Transcribe a live stream. -/
  | streaming (stream : Stream)
  /-- Wait for the whole recording. -/
  | whole
  deriving DecidableEq

/-- Decides whether two early values are equal. -/
add_decl_doc instDecidableEqEarly

/-- Compares two early values constructor by constructor. -/
add_decl_doc instDecidableEqEarly.decEq

/-- Which audio the final part reads. -/
inductive AudioMode where
  /-- The whole recording. -/
  | whole
  /-- The tail after the pieces, which may be empty. -/
  | tail (empty : Bool)
  deriving DecidableEq

/-- Decides whether two audio mode values are equal. -/
add_decl_doc instDecidableEqAudioMode

/-- Compares two audio mode values constructor by constructor. -/
add_decl_doc instDecidableEqAudioMode.decEq

/-- The final part's progress. -/
inductive Progress where
  /-- Waiting to be sent. -/
  | waiting
  /-- At the worker. -/
  | sent
  /-- Heard. -/
  | done
  deriving DecidableEq

/-- Decides whether two progress values are equal. -/
add_decl_doc instDecidableEqProgress

/-- A progress value is the constructor its index names. -/
add_decl_doc Progress.ofNat_ctorIdx

/-- The final part of a sealed recording. -/
structure Final where
  /-- What it returns. -/
  detail : Detail
  /-- Which audio it reads. -/
  mode : AudioMode
  /-- Its progress. -/
  progress : Progress
  /-- Whether it already failed once. -/
  retried : Bool

/-- A session's recognition. -/
structure Recognition where
  /-- The session it belongs to. -/
  id : Nat
  /-- What happens before the final part. -/
  early : Early
  /-- The final part, once the recording is sealed. -/
  finalPart : Option Final

namespace Recognition

/-- Fresh recognition for a session. -/
def new (id : Nat) : Recognition :=
  { id, early := .segmented [], finalPart := none }

/-- Gives up on early results: the final part reads the whole recording. -/
def whole (recognition : Recognition) : Recognition :=
  { recognition with
    early := .whole
    finalPart := recognition.finalPart.map (fun (part : Final) => { part with mode := .whole }) }

/-- A live stream became available. -/
def stream (recognition : Recognition) : Recognition :=
  match recognition.early, recognition.finalPart with
  | .segmented _, none => { recognition with early := .streaming .queued }
  | _, _ => recognition

/-- A piece arrived. Out of order, or while an earlier piece waits, it ends early results. -/
def piece (number : Nat) (recognition : Recognition) : Recognition :=
  match recognition.early with
  | .segmented pieces =>
    if recognition.finalPart.isSome then recognition
    else if number != pieces.length || pieces.contains .queued then recognition.whole
    else { recognition with early := .segmented (pieces ++ [.queued]) }
  | .streaming _ | .whole => recognition

/-- Sets a waiting final part over the given audio. -/
def sealAs (mode : AudioMode) (detail : Detail) (recognition : Recognition) : Recognition :=
  { recognition with
    finalPart := some { detail, mode, progress := .waiting, retried := false } }

/-- The recording is sealed. Text after trusted pieces reads only the tail. -/
def sealAudio (partition : Option Partition) (detail : Detail) (recognition : Recognition) :
    Recognition :=
  match recognition.early, detail, partition with
  | .streaming _, .text, _ => recognition.sealAs .whole detail
  | .segmented pieces, .text, some split =>
    if split.pieces == pieces.length && 0 < split.pieces then
      recognition.sealAs (.tail split.empty) detail
    else recognition.whole.sealAs .whole detail
  | _, _, _ => recognition.whole.sealAs .whole detail

/-- The final part is waiting to be sent. -/
def waiting (recognition : Recognition) : Bool :=
  match recognition.finalPart with
  | some part => part.progress == .waiting
  | none => false

/-- The final part is at the worker. -/
def inFlight (recognition : Recognition) : Bool :=
  match recognition.finalPart with
  | some part => part.progress == .sent
  | none => false

/-- A Show may go on without narration: its final part waits, or failed and is out again. -/
def mayLoseNarration (recognition : Recognition) : Bool :=
  match recognition.finalPart with
  | some part => part.progress == .waiting || (part.progress == .sent && part.retried)
  | none => false

/-- The index of the first piece not yet heard. -/
def firstUnheard : List Piece → Option (Nat × Piece)
  | [] => none
  | .heard :: rest => (firstUnheard rest).map (fun (found : Nat × Piece) => (found.1 + 1, found.2))
  | piece :: _ => some (0, piece)

/-- Sends the final part, cutting its tail through the environment. -/
def requestFinal (part : Final) (recognition : Recognition) (env : Env) :
    Option Request × Recognition × Env :=
  let send (mode : AudioMode) (early : Early) (env : Env) : Option Request × Recognition × Env :=
    (some { ticket := { session := recognition.id, part := .final }, source := .audio,
            detail := part.detail },
      { recognition with
        early
        finalPart := some { part with mode, progress := .sent } },
      env)
  match part.mode with
  | .tail true => (none, recognition, env)
  | .tail false =>
    let (answer, env) := env.ask .cut
    if answer == 0 then send part.mode recognition.early env else send .whole .whole env
  | .whole => send .whole recognition.early env

/-- The next transcription to send, if one is ready. -/
def request (recognition : Recognition) (env : Env) : Option Request × Recognition × Env :=
  match recognition.early with
  | .streaming .queued =>
    (some { ticket := { session := recognition.id, part := .stream }, source := .live,
            detail := .text },
      { recognition with early := .streaming .sent }, env)
  | .streaming _ => (none, recognition, env)
  | .segmented pieces =>
    match firstUnheard pieces with
    | some (_, .sent) => (none, recognition, env)
    | some (index, _) =>
      (some { ticket := { session := recognition.id, part := .piece index }, source := .audio,
              detail := .text },
        { recognition with early := .segmented (pieces.set index .sent) }, env)
    | none => match recognition.finalPart with
      | some part => if part.progress == .waiting then recognition.requestFinal part env
        else (none, recognition, env)
      | none => (none, recognition, env)
  | .whole => match recognition.finalPart with
    | some part => if part.progress == .waiting then recognition.requestFinal part env
      else (none, recognition, env)
    | none => (none, recognition, env)

/-- Whether a result with this ticket is still wanted. -/
def accepts (ticket : Ticket) (recognition : Recognition) : Bool :=
  ticket.session == recognition.id &&
    match ticket.part with
    | .stream => recognition.early == .streaming .sent
    | .piece number => match recognition.early with
      | .segmented pieces => pieces[number]? == some .sent
      | .streaming _ | .whole => false
    | .final => recognition.inFlight

/-- Joins heard pieces with a tail; the pieces are consumed. -/
def join (recognition : Recognition) : Recognition :=
  match recognition.early with
  | .segmented _ => { recognition with early := .segmented [] }
  | .streaming _ | .whole => recognition

/-- Finishes without another transcription when the early results already cover the audio. -/
def complete (recognition : Recognition) : Option Transcript × Recognition :=
  match recognition.finalPart with
  | none => (none, recognition)
  | some part =>
    if part.progress != .waiting then (none, recognition)
    else
      let finished : Recognition :=
        { recognition with finalPart := some { part with progress := .done } }
      match recognition.early with
      | .streaming .heard => (some .text, finished)
      | .segmented pieces =>
        if part.mode == .tail true && pieces.all (fun (piece : Piece) => piece == .heard) then
          (some .text, finished.join)
        else (none, recognition)
      | .streaming _ | .whole => (none, recognition)

/-- A result arrived for an accepted ticket; returns the transcript once the audio is covered. -/
def heard (ticket : Ticket) (transcript : Transcript) (recognition : Recognition) :
    Option Transcript × Recognition :=
  if !recognition.accepts ticket then (none, recognition)
  else
    match ticket.part with
    | .stream =>
      match transcript with
      | .text => { recognition with early := .streaming .heard }.complete
      | .words => (none, recognition.whole)
    | .piece number =>
      match transcript with
      | .words => (none, recognition.whole)
      | .text =>
        match recognition.early with
        | .segmented pieces =>
          if number < pieces.length then
            { recognition with early := .segmented (pieces.set number .heard) }.complete
          else (none, recognition)
        | .streaming _ | .whole => (none, recognition)
    | .final =>
      match recognition.finalPart with
      | none => (none, recognition)
      | some part =>
        let done : Recognition :=
          { recognition with finalPart := some { part with progress := .done } }
        match transcript, part.mode with
        | .text, .tail _ => (some .text, done.join)
        | transcript, _ => (some transcript, done)

/-- A transcription failed. Returns `true` when the failure is handled: stale, early, or the
final part's one retry. -/
def failed (ticket : Ticket) (recognition : Recognition) : Bool × Recognition :=
  if !recognition.accepts ticket then (true, recognition)
  else
    let whole : Recognition := recognition.whole
    match ticket.part with
    | .piece _ | .stream => (true, whole)
    | .final =>
      match whole.finalPart with
      | some part =>
        if part.retried then (false, whole)
        else (true, { whole with finalPart := some { part with retried := true,
                                                                 progress := .waiting } })
      | none => (false, whole)

end Recognition

end Speakeasy

end
