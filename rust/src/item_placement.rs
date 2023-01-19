use crate::{
    game_data::{self, Capacity, Item, ItemLocationId, Link, Map, VertexId},
    traverse::{
        apply_requirement, get_spoiler_route, is_bireachable, traverse, GlobalState, LinkIdx,
        LocalState, TraverseResult,
    },
};
use hashbrown::HashSet;
use rand::{seq::SliceRandom, Rng};
use serde_derive::{Deserialize, Serialize};
use std::{cmp::min, convert::TryFrom, iter};
use strum::VariantNames;

use crate::game_data::GameData;

#[derive(Clone)]
pub enum ItemPlacementStrategy {
    Open,
    Semiclosed,
    Closed,
}

#[derive(Clone)]
pub struct DifficultyConfig {
    pub tech: Vec<String>,
    pub shine_charge_tiles: i32,
    pub item_placement_strategy: ItemPlacementStrategy,
}

// Includes preprocessing specific to the map:
pub struct Randomizer<'a> {
    pub map: &'a Map,
    pub game_data: &'a GameData,
    pub difficulty: &'a DifficultyConfig,
    pub links: Vec<Link>,
    pub initial_items_remaining: Vec<usize>, // Corresponds to GameData.items_isv (one count per distinct item name)
}

#[derive(Clone)]
struct ItemLocationState {
    pub placed_item: Option<Item>,
    pub collected: bool,
    pub reachable: bool,
    pub bireachable: bool,
    pub bireachable_vertex_id: Option<VertexId>,
}

#[derive(Serialize, Deserialize)]
pub struct SpoilerRouteEntry {
    room: String,
    node: String,
    strat_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    energy_remaining: Option<Capacity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reserves_remaining: Option<Capacity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    missiles_remaining: Option<Capacity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    supers_remaining: Option<Capacity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    power_bombs_remaining: Option<Capacity>,
}

#[derive(Serialize, Deserialize)]
pub struct SpoilerLocation {
    // area: String,
    room: String,
    node: String,
}

#[derive(Serialize, Deserialize)]
pub struct SpoilerStartState {
    max_energy: Capacity,
    max_reserves: Capacity,
    max_missiles: Capacity,
    max_supers: Capacity,
    max_power_bombs: Capacity,
    items: Vec<String>,
    flags: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct SpoilerItemDetails {
    item: String,
    location: SpoilerLocation,
    start_state: SpoilerStartState,
    obtain_route: Vec<SpoilerRouteEntry>,
    return_route: Vec<SpoilerRouteEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct SpoilerDetails {
    step: usize,
    items: Vec<SpoilerItemDetails>,
}

#[derive(Serialize, Deserialize)]
pub struct SpoilerLog {
    details: Vec<SpoilerDetails>,
}

struct DebugData {
    forward: TraverseResult,
    reverse: TraverseResult,
}

// State that changes over the course of item placement attempts
struct RandomizationState {
    step_num: usize,
    item_precedence: Vec<Item>, // An ordering of the 21 distinct item names. The game will prioritize placing key items earlier in the list.
    item_location_state: Vec<ItemLocationState>, // Corresponds to GameData.item_locations (one record for each of 100 item locations)
    flag_bireachable: Vec<bool>,                 // Corresponds to GameData.flag_locations
    items_remaining: Vec<usize>, // Corresponds to GameData.items_isv (one count for each of 21 distinct item names)
    global_state: GlobalState,
    done: bool, // Have all key items been placed?
    debug_data: Option<DebugData>,
}

pub struct Randomization {
    pub map: Map,
    pub item_placement: Vec<Item>,
    pub spoiler_log: SpoilerLog,
}

struct SelectItemsOutput {
    key_items: Vec<Item>,
    other_items: Vec<Item>,
    new_items_remaining: Vec<usize>,
}

struct VertexInfo {
    // area_name: String,   // TODO: add this
    room_name: String,
    node_name: String,
}

impl<'r> Randomizer<'r> {
    pub fn new(
        map: &'r Map,
        difficulty: &'r DifficultyConfig,
        game_data: &'r GameData,
    ) -> Randomizer<'r> {
        let mut door_edges: Vec<(VertexId, VertexId)> = Vec::new();
        for door in &map.doors {
            let src_exit_ptr = door.0 .0;
            let src_entrance_ptr = door.0 .1;
            let dst_exit_ptr = door.1 .0;
            let dst_entrance_ptr = door.1 .1;
            let bidirectional = door.2;
            let (src_room_id, src_node_id) =
                game_data.door_ptr_pair_map[&(src_exit_ptr, src_entrance_ptr)];
            let (dst_room_id, dst_node_id) =
                game_data.door_ptr_pair_map[&(dst_exit_ptr, dst_entrance_ptr)];
            for obstacle_bitmask in 0..(1 << game_data.room_num_obstacles[&src_room_id]) {
                let src_vertex_id = game_data.vertex_isv.index_by_key
                    [&(src_room_id, src_node_id, obstacle_bitmask)];
                let dst_vertex_id =
                    game_data.vertex_isv.index_by_key[&(dst_room_id, dst_node_id, 0)];
                door_edges.push((src_vertex_id, dst_vertex_id));
            }
            if bidirectional {
                for obstacle_bitmask in 0..(1 << game_data.room_num_obstacles[&dst_room_id]) {
                    let src_vertex_id =
                        game_data.vertex_isv.index_by_key[&(src_room_id, src_node_id, 0)];
                    let dst_vertex_id = game_data.vertex_isv.index_by_key
                        [&(dst_room_id, dst_node_id, obstacle_bitmask)];
                    door_edges.push((dst_vertex_id, src_vertex_id));
                }
            }
        }
        let mut links = game_data.links.clone();
        for &(from_vertex_id, to_vertex_id) in &door_edges {
            links.push(Link {
                from_vertex_id,
                to_vertex_id,
                requirement: game_data::Requirement::Free,
                strat_name: "(Door transition)".to_string(),
            })
        }

        let mut initial_items_remaining: Vec<usize> = vec![1; game_data.item_isv.keys.len()];
        initial_items_remaining[Item::Missile as usize] = 46;
        initial_items_remaining[Item::Super as usize] = 10;
        initial_items_remaining[Item::PowerBomb as usize] = 10;
        initial_items_remaining[Item::ETank as usize] = 14;
        initial_items_remaining[Item::ReserveTank as usize] = 4;
        assert!(initial_items_remaining.iter().sum::<usize>() == game_data.item_locations.len());

        Randomizer {
            map,
            initial_items_remaining,
            game_data,
            links,
            difficulty,
        }
    }

    fn get_tech_vec(&self) -> Vec<bool> {
        let tech_set: HashSet<String> = self.difficulty.tech.iter().map(|x| x.clone()).collect();
        self.game_data
            .tech_isv
            .keys
            .iter()
            .map(|x| tech_set.contains(x))
            .collect()
    }

    fn get_flag_vec(&self) -> Vec<bool> {
        let mut flag_vec = vec![false; self.game_data.flag_isv.keys.len()];
        let tourian_open_idx = self.game_data.flag_isv.index_by_key["f_TourianOpen"];
        flag_vec[tourian_open_idx] = true;
        flag_vec
    }

    fn update_reachability(&self, state: &mut RandomizationState) {
        let num_vertices = self.game_data.vertex_isv.keys.len();
        let start_vertex_id = self.game_data.vertex_isv.index_by_key[&(8, 5, 0)]; // Landing site
        let forward = traverse(
            &self.links,
            &state.global_state,
            num_vertices,
            start_vertex_id,
            false,
            self.game_data,
        );

        // for (i, &(room_id, node_id, obstacle_bitmask)) in self.game_data.vertex_isv.keys.iter().enumerate() {
        //     if forward.local_states[i].is_some() {
        //         let room_name = &self.game_data.room_json_map[&room_id]["name"];
        //         let node_name = &self.game_data.node_json_map[&(room_id, node_id)]["name"];
        //         println!("{room_name}: {node_name} ({obstacle_bitmask})");
        //     }
        // }

        let reverse = traverse(
            &self.links,
            &state.global_state,
            num_vertices,
            start_vertex_id,
            true,
            self.game_data,
        );
        for (i, vertex_ids) in self.game_data.item_vertex_ids.iter().enumerate() {
            for &v in vertex_ids {
                if forward.local_states[v].is_some() {
                    state.item_location_state[i].reachable = true;
                    if !state.item_location_state[i].bireachable
                        && is_bireachable(
                            &state.global_state,
                            &forward.local_states[v],
                            &reverse.local_states[v],
                        )
                    {
                        state.item_location_state[i].bireachable = true;
                        state.item_location_state[i].bireachable_vertex_id = Some(v);
                    }
                }
            }
        }
        for (i, vertex_ids) in self.game_data.flag_vertex_ids.iter().enumerate() {
            for &v in vertex_ids {
                if is_bireachable(
                    &state.global_state,
                    &forward.local_states[v],
                    &reverse.local_states[v],
                ) {
                    state.flag_bireachable[i] = true;
                }
            }
        }
        // Store TraverseResults to use for constructing spoiler log
        state.debug_data = Some(DebugData { forward, reverse });
    }

    fn select_items<R: Rng>(
        &self,
        state: &RandomizationState,
        num_bireachable: usize,
        num_oneway_reachable: usize,
        attempt_num: usize,
        rng: &mut R,
    ) -> Option<SelectItemsOutput> {
        let num_items_to_place = num_bireachable + num_oneway_reachable;
        let filtered_item_precedence: Vec<Item> = state
            .item_precedence
            .iter()
            .copied()
            .filter(|&item| {
                state.items_remaining[item as usize] == self.initial_items_remaining[item as usize]
            })
            .collect();
        let num_key_items_remaining = filtered_item_precedence.len();
        let num_items_remaining: usize = state.items_remaining.iter().sum();
        let mut num_key_items_to_place = match self.difficulty.item_placement_strategy {
            ItemPlacementStrategy::Semiclosed | ItemPlacementStrategy::Closed => 1,
            ItemPlacementStrategy::Open => f32::ceil(
                (num_key_items_remaining as f32) / (num_items_remaining as f32)
                    * (num_items_to_place as f32),
            ) as usize,
        };
        if num_items_remaining < num_items_to_place + 20 {
            // println!("dumping key items");
            num_key_items_to_place = num_key_items_remaining;
        }
        num_key_items_to_place = min(
            num_key_items_to_place,
            min(num_bireachable, num_key_items_remaining),
        );
        assert!(num_key_items_to_place >= 1);
        if num_key_items_to_place - 1 + attempt_num >= num_key_items_remaining {
            return None;
        }

        // If we will be placing `k` key items, we let the first `k - 1` items to place remain fixed based on the
        // item precedence order, while we vary the last key item across attempts (to try to find some choice that
        // will expand the set of bireachable item locations).
        let mut key_items_to_place: Vec<Item> =
            filtered_item_precedence[0..(num_key_items_to_place - 1)].to_vec();
        key_items_to_place.push(filtered_item_precedence[num_key_items_to_place - 1 + attempt_num]);
        assert!(key_items_to_place.len() == num_key_items_to_place);

        let mut new_items_remaining = state.items_remaining.clone();
        for &item in &key_items_to_place {
            new_items_remaining[item as usize] -= 1;
        }

        let num_other_items_to_place = num_items_to_place - num_key_items_to_place;
        let mut item_types_to_mix: Vec<Item>;
        let mut item_types_to_delay: Vec<Item>;
        match self.difficulty.item_placement_strategy {
            ItemPlacementStrategy::Open => {
                item_types_to_mix = vec![
                    Item::Missile,
                    Item::ETank,
                    Item::ReserveTank,
                    Item::Super,
                    Item::PowerBomb,
                ];
                item_types_to_delay = vec![];
            }
            ItemPlacementStrategy::Semiclosed => {
                item_types_to_mix = vec![Item::Missile, Item::ETank, Item::ReserveTank];
                item_types_to_delay = vec![];
                if new_items_remaining[Item::Super as usize]
                    < self.initial_items_remaining[Item::Super as usize]
                {
                    item_types_to_mix.push(Item::Super);
                } else {
                    item_types_to_delay.push(Item::Super);
                }
                if new_items_remaining[Item::PowerBomb as usize]
                    < self.initial_items_remaining[Item::PowerBomb as usize]
                {
                    item_types_to_mix.push(Item::PowerBomb);
                } else {
                    item_types_to_delay.push(Item::PowerBomb);
                }
            }
            ItemPlacementStrategy::Closed => {
                item_types_to_mix = vec![Item::Missile];
                if new_items_remaining[Item::PowerBomb as usize]
                    < self.initial_items_remaining[Item::PowerBomb as usize]
                    && new_items_remaining[Item::Super as usize]
                        == self.initial_items_remaining[Item::Super as usize]
                {
                    item_types_to_delay =
                        vec![Item::ETank, Item::ReserveTank, Item::PowerBomb, Item::Super];
                } else {
                    item_types_to_delay =
                        vec![Item::ETank, Item::ReserveTank, Item::Super, Item::PowerBomb];
                }
            }
        }

        let mut items_to_mix: Vec<Item> = Vec::new();
        for &item in &item_types_to_mix {
            for _ in 0..new_items_remaining[item as usize] {
                items_to_mix.push(item);
            }
        }
        let mut expansion_items_to_delay: Vec<Item> = Vec::new();
        for &item in &item_types_to_delay {
            for _ in 0..new_items_remaining[item as usize] {
                expansion_items_to_delay.push(item);
            }
        }
        let mut key_items_to_delay: Vec<Item> = Vec::new();
        for item_id in 0..self.game_data.item_isv.keys.len() {
            let item = Item::try_from(item_id).unwrap();
            if ![
                Item::Missile,
                Item::Super,
                Item::PowerBomb,
                Item::ETank,
                Item::ReserveTank,
            ]
            .contains(&item)
            {
                key_items_to_delay.push(item);
            }
        }

        let mut other_items_to_place: Vec<Item> = items_to_mix;
        other_items_to_place.shuffle(rng);
        other_items_to_place.extend(expansion_items_to_delay);
        other_items_to_place.extend(key_items_to_delay);
        other_items_to_place = other_items_to_place[0..num_other_items_to_place].to_vec();
        for &item in &other_items_to_place {
            new_items_remaining[item as usize] -= 1;
        }
        Some(SelectItemsOutput {
            key_items: key_items_to_place,
            other_items: other_items_to_place,
            new_items_remaining,
        })
    }

    fn place_items(
        &self,
        state: &mut RandomizationState,
        bireachable_locations: &[ItemLocationId],
        other_locations: &[ItemLocationId],
        key_items_to_place: &[Item],
        other_items_to_place: &[Item],
    ) {
        let mut all_locations: Vec<ItemLocationId> = Vec::new();
        all_locations.extend(bireachable_locations);
        all_locations.extend(other_locations);
        let mut all_items_to_place: Vec<Item> = Vec::new();
        all_items_to_place.extend(key_items_to_place);
        all_items_to_place.extend(other_items_to_place);
        for (&loc, &item) in iter::zip(&all_locations, &all_items_to_place) {
            state.item_location_state[loc].placed_item = Some(item);
        }
    }

    fn collect_items(&self, state: &mut RandomizationState) {
        for item_loc_state in &mut state.item_location_state {
            if !item_loc_state.collected && item_loc_state.bireachable {
                if let Some(item) = item_loc_state.placed_item {
                    state.global_state.collect(item, self.game_data);
                    item_loc_state.collected = true;
                }
            }
        }
    }

    fn finish(&self, state: &mut RandomizationState) {
        let mut remaining_items: Vec<Item> = Vec::new();
        for item_id in 0..self.game_data.item_isv.keys.len() {
            for _ in 0..state.items_remaining[item_id] {
                remaining_items.push(Item::try_from(item_id).unwrap());
            }
        }
        let mut idx = 0;
        for item_loc_state in &mut state.item_location_state {
            if item_loc_state.placed_item.is_none() {
                item_loc_state.placed_item = Some(remaining_items[idx]);
                idx += 1;
            }
        }
        assert!(idx == remaining_items.len());
    }

    fn get_vertex_info(&self, vertex_id: usize) -> VertexInfo {
        let (room_id, node_id, _obstacle_bitmask) = self.game_data.vertex_isv.keys[vertex_id];
        VertexInfo {
            room_name: self.game_data.room_json_map[&room_id]["name"]
                .as_str()
                .unwrap()
                .to_string(),
            node_name: self.game_data.node_json_map[&(room_id, node_id)]["name"]
                .as_str()
                .unwrap()
                .to_string(),
        }
    }

    fn get_spoiler_start_state(&self, global_state: &GlobalState) -> SpoilerStartState {
        let mut items: Vec<String> = Vec::new();
        for i in 0..self.game_data.item_isv.keys.len() {
            if global_state.items[i] {
                items.push(self.game_data.item_isv.keys[i].to_string());
            }
        }
        SpoilerStartState {
            max_energy: global_state.max_energy,
            max_reserves: global_state.max_reserves,
            max_missiles: global_state.max_missiles,
            max_supers: global_state.max_supers,
            max_power_bombs: global_state.max_power_bombs,
            items: items,
            flags: vec![],
        }
    }

    fn get_spoiler_route(
        &self,
        global_state: &GlobalState,
        local_state: &mut LocalState,
        link_idxs: &[LinkIdx],
    ) -> Vec<SpoilerRouteEntry> {
        let mut route: Vec<SpoilerRouteEntry> = Vec::new();
        for &link_idx in link_idxs {
            let link = &self.links[link_idx as usize];
            let to_vertex_info = self.get_vertex_info(link.to_vertex_id);
            let new_local_state =
                apply_requirement(&link.requirement, &global_state, *local_state, false).unwrap();
            let energy_remaining: Option<Capacity> =
                if new_local_state.energy_used != local_state.energy_used {
                    Some(global_state.max_energy - new_local_state.energy_used)
                } else {
                    None
                };
            let reserves_remaining: Option<Capacity> =
                if new_local_state.reserves_used != local_state.reserves_used {
                    Some(global_state.max_reserves - new_local_state.reserves_used)
                } else {
                    None
                };
            let missiles_remaining: Option<Capacity> =
                if new_local_state.missiles_used != local_state.missiles_used {
                    Some(global_state.max_missiles - new_local_state.missiles_used)
                } else {
                    None
                };
            let supers_remaining: Option<Capacity> =
                if new_local_state.supers_used != local_state.supers_used {
                    Some(global_state.max_supers - new_local_state.supers_used)
                } else {
                    None
                };
            let power_bombs_remaining: Option<Capacity> =
                if new_local_state.power_bombs_used != local_state.power_bombs_used {
                    Some(global_state.max_power_bombs - new_local_state.power_bombs_used)
                } else {
                    None
                };

            route.push(SpoilerRouteEntry {
                room: to_vertex_info.room_name,
                node: to_vertex_info.node_name,
                strat_name: link.strat_name.clone(),
                energy_remaining,
                reserves_remaining,
                missiles_remaining,
                supers_remaining,
                power_bombs_remaining,
            });
            *local_state = new_local_state;
            // TODO: Add details about energy/ammo levels
        }
        route
    }

    fn get_spoiler_details(
        &self,
        state: &RandomizationState,
        new_state: &RandomizationState,
    ) -> SpoilerDetails {
        let mut items: Vec<SpoilerItemDetails> = Vec::new();
        for i in 0..self.game_data.item_locations.len() {
            if let Some(item) = new_state.item_location_state[i].placed_item {
                let first_item = state.items_remaining[item as usize]
                    == self.initial_items_remaining[item as usize];
                if first_item
                    && !state.item_location_state[i].collected
                    && new_state.item_location_state[i].collected
                {
                    let item_vertex_id =
                        state.item_location_state[i].bireachable_vertex_id.unwrap();
                    let forward_link_idxs: Vec<LinkIdx> = get_spoiler_route(
                        &state.debug_data.as_ref().unwrap().forward,
                        item_vertex_id,
                        false,
                    );
                    let reverse_link_idxs: Vec<LinkIdx> = get_spoiler_route(
                        &state.debug_data.as_ref().unwrap().reverse,
                        item_vertex_id,
                        true,
                    );
                    let mut local_state = LocalState::new();
                    let obtain_route = self.get_spoiler_route(
                        &state.global_state,
                        &mut local_state,
                        &forward_link_idxs,
                    );
                    let return_route = self.get_spoiler_route(
                        &state.global_state,
                        &mut local_state,
                        &reverse_link_idxs,
                    );
                    let item_vertex_info = self.get_vertex_info(item_vertex_id);
                    items.push(SpoilerItemDetails {
                        item: Item::VARIANTS[item as usize].to_string(),
                        location: SpoilerLocation {
                            // TODO: Add area
                            room: item_vertex_info.room_name,
                            node: item_vertex_info.node_name,
                        },
                        start_state: self.get_spoiler_start_state(&state.global_state),
                        obtain_route: obtain_route,
                        return_route: return_route,
                    })
                }
            }
        }
        SpoilerDetails {
            step: state.step_num,
            items,
        }
    }

    fn step<R: Rng>(&self, state: &mut RandomizationState, rng: &mut R) -> Option<SpoilerDetails> {
        loop {
            let mut any_new_flag = false;
            for i in 0..self.game_data.flag_locations.len() {
                let flag_id = self.game_data.flag_locations[i].2;
                if state.global_state.flags[flag_id] {
                    continue;
                }
                if state.flag_bireachable[i] {
                    any_new_flag = true;
                    state.global_state.flags[flag_id] = true;
                }
            }
            if any_new_flag {
                self.update_reachability(state);
            } else {
                break;
            }
        }

        let mut unplaced_bireachable: Vec<ItemLocationId> = Vec::new();
        let mut unplaced_oneway_reachable: Vec<ItemLocationId> = Vec::new();
        for (i, item_location_state) in state.item_location_state.iter().enumerate() {
            if item_location_state.placed_item.is_some() {
                continue;
            }
            if item_location_state.bireachable {
                unplaced_bireachable.push(i);
            } else if item_location_state.reachable {
                unplaced_oneway_reachable.push(i);
            }
        }
        unplaced_bireachable.shuffle(rng);
        unplaced_oneway_reachable.shuffle(rng);
        let mut attempt_num = 0;
        let mut new_state: RandomizationState;
        loop {
            let select_result = self.select_items(
                state,
                unplaced_bireachable.len(),
                unplaced_oneway_reachable.len(),
                attempt_num,
                rng,
            );
            if let Some(select_res) = select_result {
                new_state = RandomizationState {
                    step_num: state.step_num,
                    item_precedence: state.item_precedence.clone(),
                    item_location_state: state.item_location_state.clone(),
                    flag_bireachable: state.flag_bireachable.clone(),
                    items_remaining: select_res.new_items_remaining,
                    global_state: state.global_state.clone(),
                    done: false,
                    debug_data: None,
                };
                self.place_items(
                    &mut new_state,
                    &unplaced_bireachable,
                    &unplaced_oneway_reachable,
                    &select_res.key_items,
                    &select_res.other_items,
                );
                self.collect_items(&mut new_state);
                if iter::zip(&new_state.items_remaining, &self.initial_items_remaining)
                    .all(|(x, y)| x < y)
                {
                    // At least one instance of each item have been placed. This should be enough
                    // to ensure the game is beatable, so we are done.
                    new_state.done = true;
                    break;
                } else {
                    // println!("not all collected:");
                    // for (i, (x, y)) in iter::zip(&new_state.items_remaining, &self.initial_items_remaining).enumerate() {
                    //     if x >= y {
                    //         println!("item={}, remaining={x} ,initial={y}", self.game_data.item_isv.keys[i]);
                    //     }
                    // }
                    // println!("");
                }

                self.update_reachability(&mut new_state);
                if iter::zip(&new_state.item_location_state, &state.item_location_state)
                    .any(|(n, o)| n.bireachable && !o.reachable)
                {
                    // Progress: the new items unlock at least one bireachable item location that wasn't reachable before.
                    break;
                }
            } else {
                println!("Exhausted key item placement attempts");
                return None;
            }
            // println!("attempt failed");
            attempt_num += 1;
        }
        let spoiler_details = self.get_spoiler_details(state, &new_state);
        *state = new_state;
        Some(spoiler_details)
    }

    fn get_randomization(
        &self,
        state: &RandomizationState,
        spoiler_details: Vec<SpoilerDetails>,
    ) -> Randomization {
        let item_placement = state
            .item_location_state
            .iter()
            .map(|x| x.placed_item.unwrap())
            .collect();
        let spoiler_log = SpoilerLog {
            details: spoiler_details,
        };
        Randomization {
            map: self.map.clone(),
            item_placement,
            spoiler_log,
        }
    }

    pub fn randomize<R: Rng>(&self, rng: &mut R) -> Option<Randomization> {
        let initial_global_state = {
            let items = vec![false; self.game_data.item_isv.keys.len()];
            let weapon_mask = self.game_data.get_weapon_mask(&items);
            GlobalState {
                tech: self.get_tech_vec(),
                items: items,
                flags: self.get_flag_vec(),
                max_energy: 99,
                max_reserves: 0,
                max_missiles: 0,
                max_supers: 0,
                max_power_bombs: 0,
                weapon_mask: weapon_mask,
                shine_charge_tiles: self.difficulty.shine_charge_tiles,
            }
        };

        let initial_item_location_state = ItemLocationState {
            placed_item: None,
            collected: false,
            reachable: false,
            bireachable: false,
            bireachable_vertex_id: None,
        };
        let mut item_precedence: Vec<Item> = (0..self.game_data.item_isv.keys.len())
            .map(|i| Item::try_from(i).unwrap())
            .collect();
        item_precedence.shuffle(rng);
        let mut state = RandomizationState {
            step_num: 1,
            item_precedence,
            item_location_state: vec![
                initial_item_location_state;
                self.game_data.item_locations.len()
            ],
            flag_bireachable: vec![false; self.game_data.flag_locations.len()],
            items_remaining: self.initial_items_remaining.clone(),
            global_state: initial_global_state,
            done: false,
            debug_data: None,
        };
        self.update_reachability(&mut state);
        if !state.item_location_state.iter().any(|x| x.bireachable) {
            println!("No initially bireachable item locations");
            return None;
        }
        let mut spoiler_details_vec: Vec<SpoilerDetails> = Vec::new();
        while !state.done {
            let cnt_collected = state
                .item_location_state
                .iter()
                .filter(|x| x.collected)
                .count();
            let cnt_placed = state
                .item_location_state
                .iter()
                .filter(|x| x.placed_item.is_some())
                .count();
            let cnt_reachable = state
                .item_location_state
                .iter()
                .filter(|x| x.reachable)
                .count();
            let cnt_bireachable = state
                .item_location_state
                .iter()
                .filter(|x| x.bireachable)
                .count();
            println!("step={0}, bireachable={cnt_bireachable}, reachable={cnt_reachable}, placed={cnt_placed}, collected={cnt_collected}", state.step_num);
            match self.step(&mut state, rng) {
                Some(spoiler_details) => {
                    spoiler_details_vec.push(spoiler_details);
                }
                None => return None,
            }
            state.step_num += 1;
        }
        self.finish(&mut state);
        Some(self.get_randomization(&state, spoiler_details_vec))
    }
}
