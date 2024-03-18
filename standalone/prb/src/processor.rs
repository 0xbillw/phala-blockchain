use anyhow::Result;
use log::{debug, error, info, warn};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::dataprovider::{DataProviderEvent, DataProviderEventTx, WorkerSyncInfo};
use crate::pruntime::PRuntimeClient;
use crate::worker::{WorkerLifecycleCommand, WorkerLifecycleState};
use crate::worker_status::{WorkerStatusUpdate, WorkerStatusUpdateTx};

//use phactory_api::blocks::{AuthoritySetChange, HeaderToSyn};
use phactory_api::prpc::{self, Blocks, CombinedHeadersToSync, GetRuntimeInfoRequest, HeadersToSync, InitRuntimeResponse, ParaHeadersToSync, PhactoryInfo};

enum SyncStatus {
    Idle,
    Syncing,
}

pub struct WorkerContext {
    pub uuid: String,

    pub headernum: u32,
    pub para_headernum: u32,
    pub blocknum: u32,

    pub initialized: bool,
    pub registered: bool,
    pub benchmarked: bool,

    pub calling: bool,
    pub accept_sync_request: bool,

    pub client: Arc<PRuntimeClient>,
    //pub info: Option<PhactoryInfo>,
    //pub last_message: String,
    //pub session_info: Option<SessionInfo>,
    //pub pending_sequences: HashSet<u32>,
    pub pending_requests: VecDeque<PRuntimeRequest>,
}

#[derive(Clone, Default)]
pub struct SyncRequest {
    pub headers: Option<HeadersToSync>,
    pub para_headers: Option<ParaHeadersToSync>,
    pub combined_headers: Option<CombinedHeadersToSync>,
    pub blocks: Option<Blocks>,
}

#[derive(Default)]
pub struct SyncInfo {
    pub headernum: Option<u32>,
    pub para_headernum: Option<u32>,
    pub blocknum: Option<u32>,
}

#[derive(Default)]
pub struct BroadcastInfo {
    pub sync_info: SyncInfo,
    pub relay_chaintip: u32,
    pub para_chaintip: u32,
}

pub enum PRuntimeRequest {
    GetInfo,
    GetRegisterInfo((bool, Option<sp_core::crypto::AccountId32>)),
    Sync(SyncRequest),
    GetEgressMessages,
    TakeCheckpoint,
}

enum PRuntimeResponse {
    GetInfo(PhactoryInfo),
    GetRegisterInfo(InitRuntimeResponse),
    Sync(SyncInfo),
    GetEgressMessages(Vec<u8>),
    TakeCheckpoint(u32),
}

pub enum ProcessorEvent {
    Init(usize),
    Register(usize),
    GetEgressMsgTimerReceived(),
    BroadcastSyncRequest((SyncRequest, BroadcastInfo)),
    PRuntimeRequest((usize, PRuntimeRequest)),
    PRuntimeResponse((usize, Result<PRuntimeResponse, prpc::client::Error>)),
    WorkerLifecycleCommand((String, WorkerLifecycleCommand))
}

pub type ProcessorEventRx = mpsc::UnboundedReceiver<ProcessorEvent>;
pub type ProcessorEventTx = mpsc::UnboundedSender<ProcessorEvent>;

pub struct Processor {
    //workers: Vec<WorkerContext>,
    pub rx: ProcessorEventRx,
    pub tx: Arc<ProcessorEventTx>,
    pub data_provider_event_tx: Arc<DataProviderEventTx>,
    pub worker_status_update_tx: Arc<WorkerStatusUpdateTx>,

    pub relaychain_chaintip: u32,
    pub parachain_chaintip: u32,
}

impl Processor {
    pub async fn master_loop(
        &mut self,
        mut workers: Vec<WorkerContext>
    ) -> Result<()> {
        tokio::time::sleep(core::time::Duration::from_secs(20)).await;

        let mut uuid_to_worker_id = HashMap::<String, usize>::new();
        for (worker_id, worker) in workers.iter().enumerate() {
            uuid_to_worker_id.insert(worker.uuid.clone(), worker_id);
        }

        for (worker_id, _) in workers.iter().enumerate() {
            send_processor_event(self.tx.clone(), ProcessorEvent::Init(worker_id));
        }

        loop {
            let event = self.rx.recv().await;
            if event.is_none() {
                break
            }

            match event.unwrap() {
                ProcessorEvent::Init(worker_id) => {
                    let worker = workers.get_mut(worker_id).unwrap();
                    debug!("[{}] Init", worker.uuid);
                    self.add_pruntime_request(worker_id, worker, PRuntimeRequest::GetInfo).await;
                },
                ProcessorEvent::Register(_) => {
                    
                },
                ProcessorEvent::GetEgressMsgTimerReceived() => {
                    //for (worker_id, worker) in workers.iter().enumerate() {
                    //}
                },
                ProcessorEvent::BroadcastSyncRequest((request, info)) => {
                    for (worker_id, worker) in workers.iter_mut().enumerate() {
                        debug!("[{}] Looking to see BroadcastSyncRequest", worker.uuid);
                        if worker.accept_sync_request && is_match(&worker, &info.sync_info) {
                            info!("[{}] Meet BroadcastSyncRequest", worker.uuid);
                            self.add_pruntime_request(worker_id, worker, PRuntimeRequest::Sync(request.clone())).await;
                        }
                    }
                    self.relaychain_chaintip = info.relay_chaintip;
                    self.parachain_chaintip = info.para_chaintip;
                },
                ProcessorEvent::PRuntimeRequest((worker_id, request)) => {
                    let worker = workers.get_mut(worker_id).unwrap();
                    //info!("[{}] PRuntimeRequest", worker.uuid);
                    self.add_pruntime_request(worker_id, worker, request).await;
                },
                ProcessorEvent::PRuntimeResponse((worker_id, result)) => {
                    let worker = workers.get_mut(worker_id).unwrap();
                    //info!("[{}] PRuntimeResponse", worker.uuid);
                    worker.calling = false;

                    match result {
                        Ok(response) => self.handle_pruntime_response(worker_id, worker, response),
                        Err(err) => {
                            error!("[{}] met error: {}", worker.uuid, err);
                            let err_msg = format!("{}", err);
                            send_worker_status_update(
                                self.worker_status_update_tx.clone(),
                                WorkerStatusUpdate {
                                    uuid: worker.uuid.clone(),
                                    state: Some(WorkerLifecycleState::HasError(err_msg)),
                                    last_message: Some(format!("[{}] {}", chrono::offset::Local::now(), err)),
                                    ..Default::default()
                                }
                            )

                        },
                    }

                    if let Some(request) = worker.pending_requests.pop_front() {
                        self.add_pruntime_request(worker_id, worker, request).await;
                    }
                },
                ProcessorEvent::WorkerLifecycleCommand((uuid, command)) => {
                    let worker_id = uuid_to_worker_id.get(&uuid);
                    match worker_id {
                        Some(worker_id) => {
                            match command {
                                WorkerLifecycleCommand::ShouldRestart => todo!(),
                                WorkerLifecycleCommand::ShouldForceRegister => {
                                    send_processor_event(self.tx.clone(), ProcessorEvent::Register(*worker_id));
                                },
                                WorkerLifecycleCommand::ShouldUpdateEndpoint(_) => todo!(),
                            }
                        },
                        None => {
                            error!("Cannot find worker with UUID: {}", uuid);
                        },
                    }
                },
            }
        }

        Ok(())
    }


    async fn add_pruntime_request(
        &mut self,
        worker_id: usize,
        worker: &mut WorkerContext,
        request: PRuntimeRequest,
    ) {
        if let PRuntimeRequest::Sync(sync_request) = &request {
            assert!(
                worker.accept_sync_request,
                "[{}] worker does not accept sync request but received one",
                worker.uuid,
            );
            if sync_request.headers.is_none()
                && sync_request.para_headers.is_none()
                && sync_request.combined_headers.is_none()
                && sync_request.blocks.is_none()
                && (worker.blocknum < worker.para_headernum && worker.headernum <= self.relaychain_chaintip || worker.para_headernum <= self.parachain_chaintip)
            {
                warn!("[{}] Worker needs to be sync, but received an empty request. Try again.", worker.uuid);
                self.request_next_sync(worker_id, worker);
                return;
            }
            worker.accept_sync_request = false;
        }

        if worker.pending_requests.is_empty() {
            self.handle_pruntime_request(worker_id, worker, request).await;
        } else {
            worker.pending_requests.push_back(request);
        }
    }

    async fn handle_pruntime_request(
        &mut self,
        worker_id: usize,
        worker: &mut WorkerContext,
        request: PRuntimeRequest,
    ) {
        worker.calling = true;
        tokio::task::spawn(dispatch_pruntime_request(self.tx.clone(), worker_id, worker.client.clone(), request));
    }

    fn handle_pruntime_response(
        &mut self,
        worker_id: usize,
        worker: &mut WorkerContext,
        response: PRuntimeResponse,
    ) {
        match response {
            PRuntimeResponse::GetInfo(info) => {
                //info!("[{}] PRuntimeResponse, getInfo", worker.uuid);
                worker.headernum = info.headernum;
                worker.para_headernum = info.para_headernum;
                worker.blocknum = info.blocknum;
                worker.accept_sync_request = true;
                self.request_next_sync(worker_id, worker);
                send_worker_status_update(
                    self.worker_status_update_tx.clone(),
                    WorkerStatusUpdate {
                        uuid: worker.uuid.clone(),
                        state: Some(WorkerLifecycleState::Synchronizing),
                        phactory_info: Some(info),
                        ..Default::default()
                    }
                )
            },
            PRuntimeResponse::GetRegisterInfo(response) => {
                
            },
            PRuntimeResponse::Sync(info) => {
                //info!("[{}] PRuntimeResponse, sync", worker.uuid);
                worker.accept_sync_request = true;
                self.handle_pruntime_sync_response(worker_id, worker, &info);
                send_worker_status_update(
                    self.worker_status_update_tx.clone(),
                    WorkerStatusUpdate {
                        uuid: worker.uuid.clone(),
                        sync_info: Some(info),
                        ..Default::default()
                    }
                )
            },
            PRuntimeResponse::GetEgressMessages(_) => todo!(),
            PRuntimeResponse::TakeCheckpoint(_) => todo!(),
        }
    }

    fn handle_pruntime_sync_response(
        &mut self,
        worker_id: usize,
        worker: &mut WorkerContext,
        info: &SyncInfo,
    ) {
        if let Some(headernum) = info.headernum {
            worker.headernum = headernum + 1;
            debug!("[{}] updated headernum: {}", worker.uuid, worker.headernum);
        }
        if let Some(para_headernum) = info.para_headernum {
            worker.para_headernum = para_headernum + 1;
            debug!("[{}] updated para_headernum: {}", worker.uuid, worker.para_headernum);
        }
        if let Some(blocknum) = info.blocknum {
            worker.blocknum = blocknum + 1;
            debug!("[{}] updated blocknum: {}", worker.uuid, worker.blocknum);
        }

        if worker.headernum <= self.relaychain_chaintip || worker.para_headernum <= self.parachain_chaintip || worker.blocknum < worker.para_headernum {
            debug!(
                "[{}][{}] requesting next sync; {} <= {} || {} <= {} || {} <= {}",
                worker.uuid,
                worker_id,
                worker.headernum,
                self.relaychain_chaintip,
                worker.para_headernum, 
                self.parachain_chaintip,
                worker.blocknum,
                worker.para_headernum
            );
            self.request_next_sync(worker_id, worker);
        } else {
            debug!(
                "[{}] do not need to request; {} <= {} || {} <= {} || {} <= {}",
                worker.uuid,
                worker.headernum,
                self.relaychain_chaintip,
                worker.para_headernum, 
                self.parachain_chaintip,
                worker.blocknum,
                worker.para_headernum
            );
        }
    }

    fn request_next_sync(
        &mut self,
        worker_id: usize,
        worker: &WorkerContext,
    ) {
        let send_result = self.data_provider_event_tx.send(DataProviderEvent::UpdateWorkerSyncInfo(
            WorkerSyncInfo {
                worker_id,
                headernum: worker.headernum,
                para_headernum: worker.para_headernum,
                blocknum: worker.blocknum,
            }
        ));
        if let Err(send_error) = send_result {
            error!("{:?}", send_error);
            std::process::exit(255);
        }
    }

    fn handle_worker_lifecycle_command(
        &mut self,
        worker_id: usize,
        worker: &WorkerContext,
        command: WorkerLifecycleCommand,
    ) {
        match command {
            WorkerLifecycleCommand::ShouldRestart => {
                // Do we need to do anything before running init?
                send_processor_event(self.tx.clone(), ProcessorEvent::Init(worker_id));
            },
            WorkerLifecycleCommand::ShouldForceRegister => todo!(),
            WorkerLifecycleCommand::ShouldUpdateEndpoint(_) => todo!(),
        }
    }
}

async fn do_sync_request(
    client: Arc<PRuntimeClient>,
    request: SyncRequest,
) -> Result<SyncInfo, prpc::client::Error> {
    let mut response = SyncInfo { ..Default::default() };

    if let Some(headers) = request.headers {
        match client.sync_header(headers).await {
            Ok(synced_to) => {
                response.headernum = Some(synced_to.synced_to);
            },
            Err(err) => {
                return Err(err);
            },
        }
    }

    if let Some(para_headers) = request.para_headers {
        match client.sync_para_header(para_headers).await {
            Ok(synced_to) => {
                response.para_headernum = Some(synced_to.synced_to);
            },
            Err(err) => {
                return Err(err);
            },
        }
    }

    if let Some(combined_headers) = request.combined_headers {
        match client.sync_combined_headers(combined_headers).await {
            Ok(synced_to) => {
                response.headernum = Some(synced_to.relaychain_synced_to);
                response.para_headernum = Some(synced_to.parachain_synced_to);
            },
            Err(err) => {
                return Err(err);
            },
        }
    }

    if let Some(blocks) = request.blocks {
        match client.dispatch_blocks(blocks).await {
            Ok(synced_to) => {
                response.blocknum = Some(synced_to.synced_to);
            },
            Err(err) => {
                return Err(err);
            },
        }
    }

    Ok(response)
}

async fn dispatch_pruntime_request(
    tx: Arc<ProcessorEventTx>,
    worker_id: usize,
    client: Arc<PRuntimeClient>,
    request: PRuntimeRequest,
) {
    let result = match request {
        PRuntimeRequest::GetInfo => {
            //info!("dispatch pruntime request, getInfo: {}", worker_id);
            client.get_info(())
                .await
                .map(|response| PRuntimeResponse::GetInfo(response))
        },
        PRuntimeRequest::GetRegisterInfo((force_refresh_ra, operator)) => {
            let request = GetRuntimeInfoRequest::new(force_refresh_ra, operator);
            client.get_runtime_info(request)
                .await
                .map(|response| PRuntimeResponse::GetRegisterInfo(response))
        },
        PRuntimeRequest::Sync(request) => {
            //info!("dispatch pruntime request, sync: {}", worker_id);
            do_sync_request(client, request)
                .await
                .map(|response| PRuntimeResponse::Sync(response))
        },
        PRuntimeRequest::GetEgressMessages => {
            client.get_egress_messages(())
                .await
                .map(|response| {
                    PRuntimeResponse::GetEgressMessages(response.encoded_messages)
                })
        },
        PRuntimeRequest::TakeCheckpoint => todo!(),
    };

    send_processor_event(tx, ProcessorEvent::PRuntimeResponse((worker_id, result)));
}

pub fn send_processor_event(tx: Arc<ProcessorEventTx>, event: ProcessorEvent) {
    let result = tx.send(event);
    if let Err(error) = result {
        error!("{:?}", error);
        std::process::exit(255);
    }
}

fn send_worker_status_update(tx: Arc<WorkerStatusUpdateTx>, update: WorkerStatusUpdate) {
    let result = tx.send(update);
    if let Err(err) = result {
        error!("failed to update status {:?}", err);
        std::process::exit(255);
    }
}

fn is_match(worker: &WorkerContext, info: &SyncInfo) -> bool {
    debug!("[{}] start to match", worker.uuid);
    if let Some(headernum) = info.headernum {
        debug!("[{}] matching headernum: {}, {}", worker.uuid, headernum, worker.headernum);
        if headernum != worker.headernum {
            return false;
        }
    }
    if let Some(para_headernum) = info.para_headernum {
        debug!("[{}] matching para_headernum: {}, {}", worker.uuid, para_headernum, worker.para_headernum);
        if para_headernum != worker.para_headernum {
            return false;
        }
    }
    if let Some(blocknum) = info.blocknum {
        debug!("[{}] matching blocknum: {}, {}", worker.uuid, blocknum, worker.blocknum);
        if blocknum != worker.blocknum {
            return false;
        }
    }
    true
}