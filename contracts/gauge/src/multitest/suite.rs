use anyhow::Result as AnyResult;

use cosmwasm_std::{coin, to_binary, Addr, Coin, CosmosMsg, Decimal, StdResult, Uint128, WasmMsg};
use cw4::Member;
use cw4_voting::msg::InstantiateMsg as VotingInstantiateMsg;
use cw_core::msg::{
    Admin, ExecuteMsg as CoreExecuteMsg, InstantiateMsg as CoreInstantiateMsg,
    ModuleInstantiateInfo, QueryMsg as CoreQueryMsg,
};
use cw_multi_test::{App, AppResponse, ContractWrapper, Executor};
use cw_proposal_single::{
    msg::ExecuteMsg as ProposalSingleExecuteMsg,
    msg::InstantiateMsg as ProposalSingleInstantiateMsg, msg::QueryMsg as ProposalSingleQueryMsg,
    query::ProposalListResponse, state::Executor as ProposalSingleExecutor,
};
use cw_utils::Duration;
use voting::{PercentageThreshold, Threshold, Vote};

use super::adapter::{contract as adapter_contract, InstantiateMsg as AdapterInstantiateMsg};
use crate::msg::{
    ExecuteMsg, GaugeConfig, GaugeResponse, InstantiateMsg, LastExecutedSetResponse,
    ListGaugesResponse, ListOptionsResponse, ListVotesResponse, MigrateMsg, QueryMsg,
    SelectedSetResponse, VoteInfo, VoteResponse,
};

type GaugeId = u64;

pub const BLOCK_TIME: u64 = 5;

fn store_gauge(app: &mut App) -> u64 {
    let contract = Box::new(
        ContractWrapper::new_with_empty(
            crate::contract::execute,
            crate::contract::instantiate,
            crate::contract::query,
        )
        .with_migrate(crate::contract::migrate),
    );

    app.store_code(contract)
}

fn store_group(app: &mut App) -> u64 {
    let contract = Box::new(ContractWrapper::new_with_empty(
        cw4_group::contract::execute,
        cw4_group::contract::instantiate,
        cw4_group::contract::query,
    ));

    app.store_code(contract)
}

fn store_voting(app: &mut App) -> u64 {
    let contract = Box::new(
        ContractWrapper::new_with_empty(
            cw4_voting::contract::execute,
            cw4_voting::contract::instantiate,
            cw4_voting::contract::query,
        )
        .with_reply_empty(cw4_voting::contract::reply),
    );

    app.store_code(contract)
}

fn store_proposal_single(app: &mut App) -> u64 {
    let contract = Box::new(ContractWrapper::new_with_empty(
        cw_proposal_single::contract::execute,
        cw_proposal_single::contract::instantiate,
        cw_proposal_single::contract::query,
    ));

    app.store_code(contract)
}

fn store_core(app: &mut App) -> u64 {
    let contract = Box::new(
        ContractWrapper::new_with_empty(
            cw_core::contract::execute,
            cw_core::contract::instantiate,
            cw_core::contract::query,
        )
        .with_reply_empty(cw_core::contract::reply),
    );

    app.store_code(contract)
}

#[derive(Debug)]
pub struct SuiteBuilder {
    voting_members: Vec<Member>,
    initial_core_balance: Option<Coin>,
}

impl SuiteBuilder {
    pub fn new() -> Self {
        Self {
            voting_members: vec![],
            initial_core_balance: None,
        }
    }

    pub fn with_core_balance(mut self, balance: (u128, &str)) -> Self {
        self.initial_core_balance = Some(coin(balance.0, balance.1));
        self
    }

    pub fn with_voting_members(mut self, members: &[(&str, u64)]) -> Self {
        self.voting_members = members
            .iter()
            .map(|(addr, weight)| Member {
                addr: addr.to_string(),
                weight: *weight,
            })
            .collect::<Vec<Member>>();
        self
    }

    #[track_caller]
    pub fn build(self) -> Suite {
        let mut app = App::default();
        let owner = Addr::unchecked("owner");

        // instantiate cw4-voting as voting contract
        let group_code_id = store_group(&mut app);
        let voting_code_id = store_voting(&mut app);
        let voting_module = ModuleInstantiateInfo {
            code_id: voting_code_id,
            msg: to_binary(&VotingInstantiateMsg {
                cw4_group_code_id: group_code_id,
                initial_members: self.voting_members,
            })
            .unwrap(),
            admin: Admin::Address {
                addr: owner.to_string(),
            },
            label: "CW4 Voting Contract".to_owned(),
        };

        // instantiate proposal_single module
        let proposal_single_code_id = store_proposal_single(&mut app);
        let proposal_module = ModuleInstantiateInfo {
            code_id: proposal_single_code_id,
            msg: to_binary(&ProposalSingleInstantiateMsg {
                threshold: Threshold::AbsolutePercentage {
                    percentage: PercentageThreshold::Majority {},
                },
                max_voting_period: Duration::Time(66666666),
                min_voting_period: None,
                allow_revoting: false,
                deposit_info: None,
                executor: ProposalSingleExecutor::Members,
            })
            .unwrap(),
            admin: Admin::Address {
                addr: owner.to_string(),
            },
            label: "Proposal Single Contract".to_owned(),
        };

        // intantiate core contract,
        let core_code_id = store_core(&mut app);
        let core = app
            .instantiate_contract(
                core_code_id,
                owner.clone(),
                &CoreInstantiateMsg {
                    admin: Some(owner.to_string()),
                    name: "CW Core contract".to_owned(),
                    description: "Hub between voting end executing".to_owned(),
                    image_url: None,
                    automatically_add_cw20s: false,
                    automatically_add_cw721s: false,
                    voting_module_instantiate_info: voting_module,
                    proposal_modules_instantiate_info: vec![proposal_module],
                    initial_items: None,
                },
                &[],
                "CW CORE",
                None,
            )
            .unwrap();

        if let Some(core_balance) = self.initial_core_balance {
            app.init_modules(|router, _, storage| -> AnyResult<()> {
                router.bank.init_balance(storage, &core, vec![core_balance])
            })
            .unwrap();
        }

        let voting_contract: Addr = app
            .wrap()
            .query_wasm_smart(&core, &CoreQueryMsg::VotingModule {})
            .unwrap();
        let proposal_single_contract: Vec<Addr> = app
            .wrap()
            .query_wasm_smart(
                &core,
                &CoreQueryMsg::ProposalModules {
                    start_at: None,
                    limit: None,
                },
            )
            .unwrap();

        let gauge_code_id = store_gauge(&mut app);
        let gauge_adapter_code_id = app.store_code(adapter_contract());

        Suite {
            owner: owner.to_string(),
            app,
            core,
            voting: voting_contract,
            proposal_single: proposal_single_contract[0].clone(),
            gauge_code_id,
            gauge_adapter_code_id,
        }
    }
}

pub struct Suite {
    pub owner: String,
    app: App,
    core: Addr,
    voting: Addr,
    proposal_single: Addr,
    gauge_code_id: u64,
    gauge_adapter_code_id: u64,
}

impl Suite {
    pub fn advance_blocks(&mut self, blocks: u64) {
        self.app.update_block(|block| {
            block.time = block.time.plus_seconds(BLOCK_TIME * blocks);
            block.height += blocks;
        });
    }

    pub fn advance_time(&mut self, seconds: u64) {
        self.app.update_block(|block| {
            block.time = block.time.plus_seconds(seconds);
            block.height += seconds / BLOCK_TIME;
        });
    }

    pub fn next_block(&mut self) {
        self.advance_blocks(1)
    }

    pub fn current_time(&self) -> u64 {
        self.app.block_info().time.seconds()
    }

    pub fn stop_gauge(
        &mut self,
        gauge: &Addr,
        sender: impl Into<String>,
        gauge_id: u64,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            gauge.clone(),
            &ExecuteMsg::StopGauge { gauge: gauge_id },
            &[],
        )
    }

    pub fn add_option(
        &mut self,
        gauge: &Addr,
        voter: impl Into<String>,
        gauge_id: u64,
        option: impl Into<String>,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(voter),
            gauge.clone(),
            &ExecuteMsg::AddOption {
                gauge: gauge_id,
                option: option.into(),
            },
            &[],
        )
    }

    /// Helper to vote for a single option
    pub fn place_vote(
        &mut self,
        gauge: &Addr,
        voter: impl Into<String>,
        gauge_id: u64,
        option: impl Into<Option<String>>,
    ) -> AnyResult<AppResponse> {
        self.place_votes(
            gauge,
            voter,
            gauge_id,
            option.into().map(|o| vec![(o, Decimal::one())]),
        )
    }

    pub fn place_votes(
        &mut self,
        gauge: &Addr,
        voter: impl Into<String>,
        gauge_id: u64,
        votes: impl Into<Option<Vec<(String, Decimal)>>>,
    ) -> AnyResult<AppResponse> {
        let votes = votes.into().map(|v| {
            v.into_iter()
                .map(|(option, weight)| crate::state::Vote { option, weight })
                .collect::<Vec<_>>()
        });
        self.app.execute_contract(
            Addr::unchecked(voter),
            gauge.clone(),
            &ExecuteMsg::PlaceVotes {
                gauge: gauge_id,
                votes,
            },
            &[],
        )
    }

    pub fn execute_options(
        &mut self,
        gauge: &Addr,
        sender: impl Into<String>,
        gauge_id: u64,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            gauge.clone(),
            &ExecuteMsg::Execute { gauge: gauge_id },
            &[],
        )
    }

    pub fn query_gauge(&self, gauge_contract: Addr, id: u64) -> StdResult<GaugeResponse> {
        self.app
            .wrap()
            .query_wasm_smart(gauge_contract, &QueryMsg::Gauge { id })
    }

    pub fn query_gauges(&self, gauge_contract: Addr) -> StdResult<Vec<GaugeResponse>> {
        Ok(self
            .app
            .wrap()
            .query_wasm_smart::<ListGaugesResponse>(
                gauge_contract,
                &QueryMsg::ListGauges {
                    start_after: None,
                    limit: None,
                },
            )?
            .gauges)
    }

    pub fn query_selected_set(
        &self,
        gauge_contract: &Addr,
        id: u64,
    ) -> StdResult<Vec<(String, Uint128)>> {
        let set: SelectedSetResponse = self
            .app
            .wrap()
            .query_wasm_smart(gauge_contract, &QueryMsg::SelectedSet { gauge: id })?;
        Ok(set.votes)
    }

    pub fn query_last_executed_set(
        &self,
        gauge_contract: &Addr,
        id: u64,
    ) -> StdResult<Option<Vec<(String, Uint128)>>> {
        let set: LastExecutedSetResponse = self
            .app
            .wrap()
            .query_wasm_smart(gauge_contract, &QueryMsg::LastExecutedSet { gauge: id })?;
        Ok(set.votes)
    }

    pub fn query_list_options(
        &self,
        gauge_contract: &Addr,
        id: u64,
    ) -> StdResult<Vec<(String, Uint128)>> {
        let set: ListOptionsResponse = self.app.wrap().query_wasm_smart(
            gauge_contract,
            &QueryMsg::ListOptions {
                gauge: id,
                start_after: None,
                limit: None,
            },
        )?;
        Ok(set.options)
    }

    pub fn query_vote(
        &self,
        gauge_contract: &Addr,
        id: u64,
        voter: impl Into<String>,
    ) -> StdResult<Option<VoteInfo>> {
        let vote: VoteResponse = self.app.wrap().query_wasm_smart(
            gauge_contract,
            &QueryMsg::Vote {
                gauge: id,
                voter: voter.into(),
            },
        )?;
        Ok(vote.vote)
    }

    pub fn query_list_votes(&self, gauge_contract: &Addr, id: u64) -> StdResult<Vec<VoteInfo>> {
        let vote: ListVotesResponse = self.app.wrap().query_wasm_smart(
            gauge_contract,
            &QueryMsg::ListVotes {
                gauge: id,
                start_after: None,
                limit: None,
            },
        )?;
        Ok(vote.votes)
    }

    // -----------------------------------------------------

    pub fn propose_update_proposal_module(
        &mut self,
        proposer: impl Into<String>,
        gauge_config: impl Into<Option<Vec<GaugeConfig>>>,
    ) -> AnyResult<AppResponse> {
        let propose_msg = ProposalSingleExecuteMsg::Propose {
            title: "gauge as proposal module".to_owned(),
            description: "Propose core to set gauge as proposal module".to_owned(),
            msgs: vec![CosmosMsg::Wasm(WasmMsg::Execute {
                contract_addr: self.core.to_string(),
                msg: to_binary(&CoreExecuteMsg::UpdateProposalModules {
                    to_add: vec![ModuleInstantiateInfo {
                        code_id: self.gauge_code_id,
                        msg: to_binary(&InstantiateMsg {
                            voting_powers: self.voting.to_string(),
                            owner: self.owner.clone(),
                            gauges: gauge_config.into(),
                        })?,
                        admin: Admin::Address {
                            addr: self.owner.clone(),
                        },
                        label: "CW4 Voting Contract".to_owned(),
                    }],
                    to_remove: vec![],
                })?,
                funds: vec![],
            })],
        };
        self.app.execute_contract(
            Addr::unchecked(proposer),
            self.proposal_single.clone(),
            &propose_msg,
            &[],
        )
    }

    pub fn instantiate_adapter_and_create_gauge(
        &mut self,
        gauge_contract: Addr,
        options: &[&str],
        to_distribute: (u128, &str),
        max_available_percentage: impl Into<Option<Decimal>>,
    ) -> AnyResult<Addr> {
        let option = self.instantiate_adapter_and_return_config(
            options,
            to_distribute,
            max_available_percentage,
        )?;
        let gauge_adapter = option.adapter.clone();
        self.app.execute_contract(
            Addr::unchecked(&self.owner),
            gauge_contract,
            &ExecuteMsg::CreateGauge(option),
            &[],
        )?;
        Ok(Addr::unchecked(gauge_adapter))
    }

    pub fn instantiate_adapter_and_return_config(
        &mut self,
        options: &[&str],
        to_distribute: (u128, &str),
        max_available_percentage: impl Into<Option<Decimal>>,
    ) -> AnyResult<GaugeConfig> {
        let gauge_adapter = self.app.instantiate_contract(
            self.gauge_adapter_code_id,
            Addr::unchecked(&self.owner),
            &AdapterInstantiateMsg {
                options: options.iter().map(|&s| s.into()).collect(),
                to_distribute: coin(to_distribute.0, to_distribute.1),
            },
            &[],
            "gauge adapter",
            None,
        )?;

        Ok(GaugeConfig {
            title: "gauge".to_owned(),
            adapter: gauge_adapter.to_string(),
            epoch_size: 7 * 86400,
            min_percent_selected: Some(Decimal::percent(5)),
            max_options_selected: 10,
            max_available_percentage: max_available_percentage.into(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_gauge(
        &mut self,
        sender: &str,
        gauge_contract: Addr,
        gauge_id: u64,
        epoch_size: impl Into<Option<u64>>,
        min_percent_selected: Option<Decimal>,
        max_options_selected: impl Into<Option<u32>>,
        max_available_percentage: impl Into<Option<Decimal>>,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(sender),
            gauge_contract,
            &ExecuteMsg::UpdateGauge {
                gauge_id,
                epoch_size: epoch_size.into(),
                min_percent_selected,
                max_options_selected: max_options_selected.into(),
                max_available_percentage: max_available_percentage.into(),
            },
            &[],
        )
    }

    pub fn place_vote_single(
        &mut self,
        voter: impl Into<String>,
        proposal_id: u64,
        vote: Vote,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(voter),
            self.proposal_single.clone(),
            &ProposalSingleExecuteMsg::Vote { proposal_id, vote },
            &[],
        )
    }

    pub fn execute_single_proposal(
        &mut self,
        executor: impl Into<String>,
        proposal_id: u64,
    ) -> AnyResult<AppResponse> {
        self.app.execute_contract(
            Addr::unchecked(executor),
            self.proposal_single.clone(),
            &ProposalSingleExecuteMsg::Execute { proposal_id },
            &[],
        )
    }

    pub fn list_proposals(&self) -> StdResult<Vec<u64>> {
        let list: ProposalListResponse = self.app.wrap().query_wasm_smart(
            self.proposal_single.clone(),
            &ProposalSingleQueryMsg::ListProposals {
                start_after: None,
                limit: None,
            },
        )?;
        Ok(list.proposals.into_iter().map(|prop| prop.id).collect())
    }

    pub fn query_proposal_modules(&self) -> StdResult<Vec<Addr>> {
        self.app.wrap().query_wasm_smart(
            self.core.clone(),
            &CoreQueryMsg::ProposalModules {
                start_at: None,
                limit: None,
            },
        )
    }

    pub fn query_balance(&self, account: &str, denom: &str) -> StdResult<u128> {
        let balance = self.app.wrap().query_balance(account, denom)?;
        Ok(balance.amount.u128())
    }

    pub fn auto_migrate_gauge(
        &mut self,
        gauge: &Addr,
        next_epochs: impl Into<Option<Vec<(GaugeId, u64)>>>,
    ) -> AnyResult<AppResponse> {
        let sender = Addr::unchecked(&self.owner);
        self.app.migrate_contract(
            sender,
            gauge.clone(),
            &MigrateMsg {
                next_epochs: next_epochs.into(),
            },
            self.gauge_code_id,
        )
    }
}
