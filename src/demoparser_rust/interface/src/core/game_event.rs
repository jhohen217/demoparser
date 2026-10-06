use crate::models::kill::Kill;

#[derive(Debug, Clone)]
pub struct TeamChange {
    pub tick: i32,
    pub user_steamid: u64,
    pub team_number: i32,
}

#[derive(Debug, Clone)]
pub enum GameEvent {
    Kill(Kill),
    TeamChange(TeamChange),
}
