#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use syncfast_core::bisync::ReviewKind;
use syncfast_core::bisync_log::RunSummary;
use syncfast_core::ops::FirstRound;
use syncfast_core::preview::Preview;
use syncfast_core::state::{DisplayStatus, GateKind};
use syncfast_gui::backend::{Command, CreatedSync, Event};
use syncfast_gui::sheet_io::PreviewFailure;
use syncfast_gui::sheets::{Sheets, Surroundings};
use syncfast_gui::ui::account_sheet::{AccountAction, AccountMode, ConnectRequest, Secret};
use syncfast_gui::ui::add_sync::{AddSyncAction, NewSync};
use syncfast_gui::ui::review::ReviewAction;
use syncfast_gui::ui::{Action, PairRow, RemoteRow};

fn sheets() -> Sheets {
    Sheets::new("/home/ada".into(), "/home/ada/.local/share/syncfast".into())
}

fn around(pairs: &[PairRow], welcome: bool) -> Surroundings<'_> {
    Surroundings { pairs, welcome }
}

fn pair(id: &str, gate: Option<GateKind>) -> PairRow {
    PairRow {
        id: id.into(),
        name: id.to_uppercase(),
        local: format!("/home/ada/{id}").into(),
        remote: format!("gdrive:{id}"),
        enabled: true,
        status: DisplayStatus::NeedsReview,
        gate,
        last_success: None,
        progress: None,
        gate_detail: String::new(),
        gate_fault: None,
        consecutive_failures: 0,
        realtime_off: false,
        presets: Vec::new(),
        filters: Vec::new(),
        effective: syncfast_core::settings::Effective::default(),
    }
}

fn found(hash: &str) -> Preview {
    Preview {
        filters_hash: hash.into(),
        excluded_repos: Vec::new(),
        summary: RunSummary::default(),
    }
}

fn sync_created(first_round: FirstRound) -> Event {
    Event::SyncCreated(Ok(CreatedSync {
        id: "dev".into(),
        first_round,
    }))
}

fn new_sync() -> NewSync {
    NewSync {
        id: "dev".into(),
        name: "Dev".into(),
        local: PathBuf::from("/home/ada/dev"),
        remote: "devremote:".into(),
        presets: vec!["system_junk".into()],
        filters: Vec::new(),
    }
}

fn connect_request(name: &str) -> ConnectRequest {
    ConnectRequest {
        mode: AccountMode::Add,
        name: name.into(),
        client_id: "id".into(),
        client_secret: Secret::new("secret".into()),
    }
}

#[test]
fn opening_the_review_of_a_sync_waiting_for_review_asks_for_the_matching_preview() {
    let cases = [
        (GateKind::NeedsReviewFirstRun, ReviewKind::Resync),
        (GateKind::NeedsReviewFiltersChanged, ReviewKind::Resync),
        (GateKind::NeedsReviewCriticalError, ReviewKind::Resync),
        (GateKind::TooManyDeletes, ReviewKind::Force),
        (GateKind::AllFilesChanged, ReviewKind::Force),
    ];
    for (gate, kind) in cases {
        let mut sheets = sheets();
        let pairs = [pair("notes", Some(gate))];

        let commands = sheets.apply(&Action::OpenReview("notes".into()), &around(&pairs, false));

        assert_eq!(
            commands,
            [Command::Preview {
                pair_id: "notes".into(),
                kind
            }],
            "{gate:?}"
        );
        assert!(sheets.is_open());
    }
}

#[test]
fn a_review_of_a_sync_without_a_review_gate_or_unknown_opens_nothing() {
    let mut sheets = sheets();
    let pairs = [pair("auth", Some(GateKind::AuthError)), pair("fine", None)];

    for id in ["auth", "fine", "gone"] {
        let commands = sheets.apply(&Action::OpenReview(id.into()), &around(&pairs, false));

        assert_eq!(commands, [], "{id}");
        assert!(!sheets.is_open(), "{id}");
    }
}

#[test]
fn confirming_a_ready_review_leaves_the_marker_of_that_preview_and_closes_the_sheet() {
    let mut sheets = sheets();
    let pairs = [pair("notes", Some(GateKind::TooManyDeletes))];
    sheets.apply(&Action::OpenReview("notes".into()), &around(&pairs, false));
    sheets.handle_event(&Event::Preview {
        pair_id: "notes".into(),
        result: Ok(found("abc123")),
    });

    let commands = sheets.apply(
        &Action::Review(ReviewAction::Confirm),
        &around(&pairs, false),
    );

    assert_eq!(
        commands,
        [Command::ConfirmReview {
            pair_id: "notes".into(),
            kind: ReviewKind::Force,
            filters_hash: "abc123".into()
        }]
    );
    assert!(!sheets.is_open());
}

#[test]
fn confirming_before_the_preview_arrived_does_nothing() {
    let mut sheets = sheets();
    let pairs = [pair("notes", Some(GateKind::NeedsReviewFirstRun))];
    sheets.apply(&Action::OpenReview("notes".into()), &around(&pairs, false));

    let commands = sheets.apply(
        &Action::Review(ReviewAction::Confirm),
        &around(&pairs, false),
    );

    assert_eq!(commands, []);
    assert!(sheets.is_open());
}

#[test]
fn cancelling_a_review_stops_the_dry_run_and_closes_the_sheet() {
    let mut sheets = sheets();
    let pairs = [pair("notes", Some(GateKind::NeedsReviewFirstRun))];
    sheets.apply(&Action::OpenReview("notes".into()), &around(&pairs, false));

    let commands = sheets.apply(
        &Action::Review(ReviewAction::Cancel),
        &around(&pairs, false),
    );

    assert_eq!(commands, [Command::CancelPreview]);
    assert!(!sheets.is_open());
}

#[test]
fn a_busy_review_asks_again_five_seconds_later_and_not_before() {
    let mut sheets = sheets();
    let pairs = [pair("notes", Some(GateKind::NeedsReviewFirstRun))];
    sheets.apply(&Action::OpenReview("notes".into()), &around(&pairs, false));
    sheets.handle_event(&Event::Preview {
        pair_id: "notes".into(),
        result: Err(PreviewFailure::Busy),
    });
    let busy_at = Instant::now();

    let (early, wait) = sheets.tick(busy_at + Duration::from_secs(1));
    let (due, after) = sheets.tick(busy_at + Duration::from_secs(6));

    assert_eq!(early, []);
    assert!(wait.is_some_and(|wait| wait <= Duration::from_secs(5)));
    assert_eq!(
        due,
        [Command::Preview {
            pair_id: "notes".into(),
            kind: ReviewKind::Resync
        }]
    );
    assert_eq!(after, None);
}

#[test]
fn a_failed_preview_is_asked_again_only_when_the_user_says_try_again() {
    let mut sheets = sheets();
    let pairs = [pair("notes", Some(GateKind::TooManyDeletes))];
    sheets.apply(&Action::OpenReview("notes".into()), &around(&pairs, false));
    sheets.handle_event(&Event::Preview {
        pair_id: "notes".into(),
        result: Err(PreviewFailure::Failed("boom".into())),
    });

    let (idle, _) = sheets.tick(Instant::now() + Duration::from_secs(60));
    let retry = sheets.apply(&Action::Review(ReviewAction::Retry), &around(&pairs, false));

    assert_eq!(idle, []);
    assert_eq!(
        retry,
        [Command::Preview {
            pair_id: "notes".into(),
            kind: ReviewKind::Force
        }]
    );
}

#[test]
fn a_preview_of_another_sync_does_not_change_the_open_review() {
    let mut sheets = sheets();
    let pairs = [pair("notes", Some(GateKind::NeedsReviewFirstRun))];
    sheets.apply(&Action::OpenReview("notes".into()), &around(&pairs, false));

    sheets.handle_event(&Event::Preview {
        pair_id: "other".into(),
        result: Err(PreviewFailure::Busy),
    });
    let (commands, wait) = sheets.tick(Instant::now() + Duration::from_secs(60));

    assert_eq!(commands, []);
    assert_eq!(wait, None);
}

#[test]
fn creating_a_sync_sends_it_to_the_backend_and_then_opens_its_first_review() {
    let mut sheets = sheets();
    sheets.apply(&Action::AddSync, &around(&[], false));

    let created = sheets.apply(
        &Action::AddSyncSheet(AddSyncAction::Create(new_sync())),
        &around(&[], false),
    );
    let review = sheets.handle_event(&sync_created(FirstRound::Held));

    assert_eq!(created, [Command::CreateSync(new_sync())]);
    assert_eq!(
        review,
        [Command::Preview {
            pair_id: "dev".into(),
            kind: ReviewKind::Resync
        }]
    );
}

#[test]
fn a_sync_confirmed_automatically_closes_the_sheet_and_opens_no_review() {
    let mut sheets = sheets();
    sheets.apply(&Action::AddSync, &around(&[], false));
    sheets.apply(
        &Action::AddSyncSheet(AddSyncAction::Create(new_sync())),
        &around(&[], false),
    );

    let commands = sheets.handle_event(&sync_created(FirstRound::Confirmed));

    assert_eq!(commands, []);
    assert!(!sheets.is_open());
}

#[test]
fn a_sync_whose_first_check_is_still_running_closes_the_sheet_and_opens_no_review() {
    let mut sheets = sheets();
    sheets.apply(&Action::AddSync, &around(&[], false));
    sheets.apply(
        &Action::AddSyncSheet(AddSyncAction::Create(new_sync())),
        &around(&[], false),
    );

    let commands = sheets.handle_event(&sync_created(FirstRound::StillChecking));

    assert_eq!(commands, []);
    assert!(!sheets.is_open());
}

#[test]
fn a_sync_the_config_refuses_keeps_the_sheet_open_for_another_try() {
    let mut sheets = sheets();
    sheets.apply(&Action::AddSync, &around(&[], false));
    sheets.apply(
        &Action::AddSyncSheet(AddSyncAction::Create(new_sync())),
        &around(&[], false),
    );

    let commands = sheets.handle_event(&Event::SyncCreated(Err(
        "local folder overlaps \"Notes\"".into()
    )));
    let again = sheets.apply(
        &Action::AddSyncSheet(AddSyncAction::Create(new_sync())),
        &around(&[], false),
    );

    assert_eq!(commands, []);
    assert_eq!(again, [Command::CreateSync(new_sync())]);
    assert!(sheets.is_open());
}

#[test]
fn get_started_without_an_account_chains_account_then_add_sync_then_review() {
    let mut sheets = sheets();

    let account = sheets.apply(&Action::AddAccount, &around(&[], true));
    let signing_in = sheets.apply(
        &Action::Account(AccountAction::Connect(connect_request("gdrive"))),
        &around(&[], true),
    );
    let remotes = sheets.handle_event(&Event::Remotes(vec![RemoteRow {
        name: "gdrive".into(),
        kind: "drive".into(),
        own_client_id: true,
    }]));
    let wizard = sheets.handle_event(&Event::Connected(Ok("gdrive".into())));
    let created = sheets.apply(
        &Action::AddSyncSheet(AddSyncAction::Create(new_sync())),
        &around(&[], true),
    );
    let review = sheets.handle_event(&sync_created(FirstRound::Held));

    assert_eq!(account, [Command::LoadRemotes]);
    assert_eq!(
        signing_in,
        [Command::ConnectAccount(connect_request("gdrive"))]
    );
    assert_eq!(remotes, []);
    assert_eq!(sheets.remotes().len(), 1);
    assert_eq!(wizard, [Command::LoadRemotes]);
    assert_eq!(created, [Command::CreateSync(new_sync())]);
    assert!(matches!(review.as_slice(), [Command::Preview { .. }]));
}

#[test]
fn an_account_added_from_settings_does_not_open_add_sync() {
    let mut sheets = sheets();
    let pairs = [pair("notes", None)];
    sheets.apply(&Action::AddAccount, &around(&pairs, false));
    sheets.apply(
        &Action::Account(AccountAction::Connect(connect_request("work"))),
        &around(&pairs, false),
    );

    let commands = sheets.handle_event(&Event::Connected(Ok("work".into())));

    assert_eq!(commands, []);
    assert!(!sheets.is_open());
}

#[test]
fn a_failed_sign_in_keeps_the_sheet_so_the_user_can_try_again() {
    let mut sheets = sheets();
    sheets.apply(&Action::AddAccount, &around(&[], true));
    sheets.apply(
        &Action::Account(AccountAction::Connect(connect_request("work"))),
        &around(&[], true),
    );

    let commands = sheets.handle_event(&Event::Connected(Err("access_denied".into())));
    let retry = sheets.apply(
        &Action::Account(AccountAction::Connect(connect_request("work"))),
        &around(&[], true),
    );

    assert_eq!(commands, []);
    assert_eq!(retry, [Command::ConnectAccount(connect_request("work"))]);
}

#[test]
fn cancelling_a_sign_in_kills_it_and_brings_the_wizard_back_when_it_came_from_there() {
    let mut sheets = sheets();
    sheets.apply(&Action::AddSync, &around(&[], false));
    sheets.apply(&Action::AddAccount, &around(&[], false));
    sheets.apply(
        &Action::Account(AccountAction::Connect(connect_request("work"))),
        &around(&[], false),
    );

    let commands = sheets.apply(&Action::Account(AccountAction::Cancel), &around(&[], false));

    assert_eq!(commands, [Command::CancelConnect, Command::LoadRemotes]);
    assert!(sheets.is_open());
}

#[test]
fn cancelling_the_account_of_the_welcome_screen_closes_everything() {
    let mut sheets = sheets();
    sheets.apply(&Action::AddAccount, &around(&[], true));

    let commands = sheets.apply(&Action::Account(AccountAction::Cancel), &around(&[], true));

    assert_eq!(commands, []);
    assert!(!sheets.is_open());
}

#[test]
fn reconnecting_an_account_asks_the_backend_for_its_saved_client() {
    let mut sheets = sheets();

    let commands = sheets.apply(
        &Action::ReconnectAccount("gdrive".into()),
        &around(&[], false),
    );

    assert_eq!(
        commands,
        [
            Command::LoadRemotes,
            Command::LoadCredentials("gdrive".into())
        ]
    );
}

#[test]
fn giving_an_account_its_own_client_id_also_asks_for_the_saved_client() {
    let mut sheets = sheets();

    let commands = sheets.apply(
        &Action::ChangeClientId("gdrive".into()),
        &around(&[], false),
    );

    assert_eq!(
        commands,
        [
            Command::LoadRemotes,
            Command::LoadCredentials("gdrive".into())
        ]
    );
}

#[test]
fn renaming_an_account_asks_for_the_saved_client_of_the_old_name() {
    let mut sheets = sheets();

    let commands = sheets.apply(&Action::RenameAccount("gdrive".into()), &around(&[], false));

    assert_eq!(
        commands,
        [
            Command::LoadRemotes,
            Command::LoadCredentials("gdrive".into())
        ]
    );
    assert!(sheets.is_open());
}

#[test]
fn a_finished_rename_closes_the_sheet_even_on_the_welcome_screen() {
    let mut sheets = sheets();
    sheets.apply(&Action::RenameAccount("gdrive".into()), &around(&[], true));

    let commands = sheets.handle_event(&Event::Connected(Ok("personal".into())));

    assert!(commands.is_empty());
    assert!(!sheets.is_open());
}
